//! Bounded discovery for the model; the editor's full control-channel catalog stays unchanged.
use crate::{ToolCall, bounds};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PAGE_BYTES: usize = 24 * 1024;
const MAX_COMMANDS: usize = 8192;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Query {
    filter: String,
    filters: Vec<String>,
    refresh: bool,
    offset: usize,
    limit: Option<usize>,
}
fn query(params: &Value) -> Result<Query, String> {
    if !params.is_object() {
        return Err("Command discovery parameters must be a JSON object".into());
    }
    let mut q: Query = serde_json::from_value(params.clone())
        .map_err(|_| "engine.commands expects {filter?: string OR filters?: [string], offset?: integer, limit?: integer, refresh?: bool}; use filter/filters, not search/query")?;
    if q.filter.len() > 256
        || q.offset > MAX_COMMANDS
        || q.limit.is_some_and(|n| !(1..=32).contains(&n))
        || q.filters.len() > 8
        || q.filters
            .iter()
            .any(|f| f.trim().is_empty() || f.len() > 256)
        || (!q.filter.trim().is_empty() && !q.filters.is_empty())
    {
        return Err(
            "Use filter OR up to 8 nonempty filters (<=256 bytes each), offset <=8192 and limit 1..32".into(),
        );
    }
    q.filter = q.filter.trim().to_owned();
    for filter in &mut q.filters {
        *filter = filter.trim().to_owned();
    }
    Ok(q)
}
pub(crate) fn is_index(call: &ToolCall) -> bool {
    call.arguments["method"] == "engine.commands"
        && call.arguments["params_json"]
            .as_str()
            .filter(|s| s.len() <= 64 * 1024)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|p| query(&p).ok())
            .is_some_and(|q| q.filter.trim().is_empty() && q.filters.is_empty())
}
pub(super) fn prepare(method: &str, params: &mut Value) -> Result<(), String> {
    if method == "engine.commands" {
        let q = query(params)?;
        // Paging belongs to the AI adapter; old editor registries only understand `filter`.
        if let Some(params) = params.as_object_mut() {
            params.remove("filters");
            if !q.filters.is_empty() {
                // Multi-topic search is local: ordinary editor registries accept one substring.
                params.remove("filter");
            } else if params.contains_key("filter") {
                params.insert("filter".into(), json!(q.filter));
            }
            params.remove("offset");
            params.remove("limit");
            params.remove("refresh");
        }
    }
    Ok(())
}

pub(crate) fn compact(call: &ToolCall, result: Value) -> Result<Value, String> {
    if call.arguments["method"] != "engine.commands" {
        return Ok(result);
    }
    let Some(params) = call.arguments["params_json"].as_str() else {
        return Ok(result);
    };
    if params.len() > 64 * 1024 {
        return Err("Editor parameters exceed 64 KiB".into());
    }
    let params: Value =
        serde_json::from_str(params).map_err(|_| "Invalid command discovery parameters")?;
    let q = query(&params)?;
    bounds::value_size(&result, crate::MAX_REPLY)?;
    let string = result.is_string();
    let mut decoded = if let Some(text) = result.as_str() {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Ok(result);
        };
        value
    } else {
        result.clone()
    };
    let wrapped = decoded.is_object() && decoded["ok"] == true;
    let commands = if wrapped {
        decoded["result"].as_array()
    } else {
        decoded.as_array()
    };
    let Some(commands) = commands else {
        return Ok(result);
    };
    // Images, failures, or another host-specific reply are not command metadata.
    if !commands.iter().all(|c| c["id"].is_string()) {
        return Ok(result);
    }
    if commands.len() > MAX_COMMANDS {
        return Err("Command catalog exceeds 8192 entries; request a narrower filter".into());
    }
    let filters: Vec<_> = if q.filters.is_empty() {
        vec![q.filter.to_ascii_lowercase()]
    } else {
        q.filters.iter().map(|f| f.to_ascii_lowercase()).collect()
    };
    // Some hosts return the entire catalog even with `filter`. Apply it here before paging,
    // so the model never loops over unrelated schemas while looking for the requested command.
    let mut commands: Vec<_> = commands
        .iter()
        .filter(|c| {
            filters.iter().any(|f| {
                c["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains(f)
                    || c["label"]
                        .as_str()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(f)
            })
        })
        .collect();
    // An exact command requested alongside a broad topic must not be hidden behind that
    // topic's many siblings. This lets one bounded page answer mixed workflow queries.
    commands.sort_by_key(|c| {
        filters
            .iter()
            .position(|f| {
                c["id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(f))
            })
            .map_or((1, 0), |index| (0, index))
    });
    let page = if q.filter.trim().is_empty() && q.filters.is_empty() {
        let mut groups: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
        for command in &commands {
            let Some(id) = command["id"].as_str().filter(|id| id.len() <= 256) else {
                return Err("Invalid command ID in catalog".into());
            };
            let prefix = id.split_once('.').map_or(id, |(prefix, _)| prefix);
            let group = groups.entry(prefix).or_default();
            group.0 += 1;
            if group.1.len() < 2 {
                group.1.push(id);
            }
            if groups.len() > 128 {
                return Err("Too many command topics; request a filter".into());
            }
        }
        json!({"total":commands.len(),"topics":groups.into_iter().map(|(prefix,(count,examples))|json!({"filter":prefix,"count":count,"examples":examples})).collect::<Vec<_>>(),
            "instructions":"This is a topic index, not executable parameter schemas. Call engine.commands with params_json {\"filter\":\"topic or exact command ID\"} for full schemas. Filters match ID or label substrings. Do not repeat the full index. Run discovered commands through engine.execute {command,params}."})
    } else {
        let mut page = Vec::new();
        for command in commands.iter().skip(q.offset).take(q.limit.unwrap_or(16)) {
            bounds::value_size(command, PAGE_BYTES / 2)?;
            page.push((**command).clone());
            if bounds::serialized_size(&page, PAGE_BYTES / 2).is_err() {
                page.pop();
                break;
            }
        }
        if page.is_empty() && q.offset < commands.len() {
            return Err(
                "Command schema is too large for discovery; request an exact command ID".into(),
            );
        }
        let next = q.offset.saturating_add(page.len());
        let mut next_query = if q.filters.is_empty() {
            json!({"filter":q.filter})
        } else {
            json!({"filters":q.filters})
        };
        next_query["offset"] = json!(next);
        next_query["limit"] = json!(q.limit.unwrap_or(16));
        json!({"filter":q.filter,"filters":q.filters,"total":commands.len(),"offset":q.offset,"commands":page,
            "next":(next<commands.len()).then_some(next_query),
            "instructions":"Exact command parameters. Reuse these schemas across messages; do not rediscover them for every question. Use engine.execute {command,params}. Request next only if a required command is missing, not to enumerate the whole editor. enabled/why describe only the moment of discovery; inspect live state before editing."})
    };
    bounds::value_size(&page, PAGE_BYTES)?;
    if wrapped {
        decoded["result"] = page;
    } else {
        decoded = page;
    }
    Ok(if string {
        Value::String(decoded.to_string())
    } else {
        decoded
    })
}

pub(crate) fn reuse(call: &ToolCall, items: &[Value]) -> Result<Option<Value>, String> {
    if call.arguments["method"] != "engine.commands" {
        return Ok(None);
    }
    // A cached answer must validate the same envelope as EditorTools::parse; otherwise
    // an extra or mistyped argument would be accepted only after the first discovery.
    if !call
        .arguments
        .as_object()
        .is_some_and(|args| args.len() == 2 && args.contains_key("params_json"))
    {
        return Err("Expected only method and params_json strings".into());
    }
    let text = call.arguments["params_json"]
        .as_str()
        .filter(|s| s.len() <= 64 * 1024)
        .ok_or("Expected params_json <=64 KiB")?;
    let params: Value =
        serde_json::from_str(text).map_err(|_| "Invalid command discovery parameters")?;
    let q = query(&params)?;
    Ok(if q.refresh {
        None
    } else {
        super::knowledge::cached(call, items)
    })
}

/// Old saved catalogs are reproducible read-only metadata, rather than irreplaceable user work.
/// Keep their call/output IDs paired and every non-catalog item intact when reducing them.
pub(crate) fn reduce_history(items: &mut [Value]) {
    let mut calls = BTreeMap::new();
    for item in items {
        if item["type"] == "function_call" {
            if let (Some(id), Some(name), Some(args)) = (
                item["call_id"].as_str(),
                item["name"].as_str(),
                item["arguments"].as_str(),
            ) && args.len() <= 64 * 1024
                && let Ok(args) = serde_json::from_str::<Value>(args)
                && args["method"] == "engine.commands"
            {
                calls.insert(id.to_owned(), ToolCall::new(name, args));
            }
        } else if item["type"] == "function_call_output"
            && let Some(call) = item["call_id"].as_str().and_then(|id| calls.get(id))
            && let Ok(reduced) = compact(call, item["output"].clone())
        {
            item["output"] = reduced;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(params: Value) -> ToolCall {
        ToolCall::new(
            "soundcraft",
            json!({"method":"engine.commands","params_json":params.to_string()}),
        )
    }
    fn catalog() -> Value {
        json!({"ok":true,"result":(0..80).map(|i|json!({"id":format!("synth.param{i}"),"params":"{track,value}","enabled":i%2==0})).collect::<Vec<_>>()})
    }
    #[test]
    fn filtered_pages_keep_whole_schemas_and_provide_the_next_query() {
        let page = compact(&call(json!({"filter":"synth","offset":16})), catalog()).unwrap();
        assert_eq!(page["result"]["commands"].as_array().unwrap().len(), 16);
        assert_eq!(page["result"]["commands"][0]["id"], "synth.param16");
        assert_eq!(page["result"]["commands"][0]["params"], "{track,value}");
        assert_eq!(page["result"]["next"]["offset"], 32);
        assert_eq!(page["result"]["commands"][1]["enabled"], false);
        assert!(serde_json::to_vec(&page).unwrap().len() < PAGE_BYTES);
    }
    #[test]
    fn bad_discovery_parameters_are_rejected_before_host_dispatch() {
        for mut params in [
            json!({"search":"synth"}),
            json!({"filter":42}),
            json!({"offset":-1}),
            json!({"limit":0}),
            json!({"limit":33}),
            json!({"filter":"x".repeat(257)}),
            json!({"filters":[""]}),
            json!({"filter":"synth","filters":["soundlab"]}),
            json!({"filters":["a","b","c","d","e","f","g","h","i"]}),
            json!({"filters":[42]}),
            json!({"refresh":"yes"}),
        ] {
            assert!(prepare("engine.commands", &mut params).is_err());
        }
        let mut params = json!({"filter":"soundlab","offset":16,"limit":8});
        prepare("engine.commands", &mut params).unwrap();
        assert_eq!(params, json!({"filter":"soundlab"}));
        let mut params = json!({"filters":["soundlab.preview","synth.catalog"],"refresh":true});
        prepare("engine.commands", &mut params).unwrap();
        assert_eq!(params, json!({}));
    }
    #[test]
    fn filtering_is_local_even_when_the_host_returns_every_command() {
        let result = json!({"ok":true,"result":[
            {"id":"file.new","label":"New","params":"{}"},
            {"id":"soundlab.preview","label":"FORGE Preview","params":"{track,patch,base_revision}"},
            {"id":"synth.catalog","label":"Synth Catalog","params":"{}"}
        ]});
        let page = compact(&call(json!({"filter":"soundlab.preview"})), result.clone()).unwrap();
        assert_eq!(page["result"]["total"], 1);
        assert_eq!(page["result"]["commands"][0]["id"], "soundlab.preview");
        let page = compact(
            &call(json!({"filters":["soundlab.preview","synth.catalog"]})),
            result,
        )
        .unwrap();
        assert_eq!(page["result"]["total"], 2);
        assert_eq!(page["result"]["commands"][1]["id"], "synth.catalog");
        assert!(page["result"]["next"].is_null());
    }
    #[test]
    fn errors_images_and_non_discovery_results_are_not_rewritten() {
        for result in [
            json!({"ok":false,"error":"Denied"}),
            json!([{"type":"input_text","text":"image metadata"}]),
            json!("not JSON"),
        ] {
            assert_eq!(compact(&call(json!({})), result.clone()).unwrap(), result);
        }
        let call = ToolCall::new(
            "soundcraft",
            json!({"method":"engine.execute","params_json":"{}"}),
        );
        let result = catalog();
        assert_eq!(compact(&call, result.clone()).unwrap(), result);
    }
}
