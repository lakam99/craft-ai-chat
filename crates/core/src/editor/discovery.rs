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
    offset: usize,
    limit: Option<usize>,
}
fn query(params: &Value) -> Result<Query, String> {
    if !params.is_object() {
        return Err("Command discovery parameters must be a JSON object".into());
    }
    let q: Query = serde_json::from_value(params.clone())
        .map_err(|_| "engine.commands expects {filter?: string, offset?: integer, limit?: integer}; use filter, not search/query")?;
    if q.filter.len() > 256
        || q.offset > MAX_COMMANDS
        || q.limit.is_some_and(|n| !(1..=32).contains(&n))
    {
        return Err(
            "Command discovery requires filter <=256 bytes, offset <=8192 and limit 1..32".into(),
        );
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
            .is_some_and(|q| q.filter.trim().is_empty())
}
pub(super) fn prepare(method: &str, params: &mut Value) -> Result<(), String> {
    if method == "engine.commands" {
        query(params)?;
        // Paging belongs to the AI adapter; old editor registries only understand `filter`.
        if let Some(params) = params.as_object_mut() {
            params.remove("offset");
            params.remove("limit");
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
    let page = if q.filter.trim().is_empty() {
        let mut groups: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
        for command in commands {
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
            page.push(command.clone());
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
        json!({"filter":q.filter,"total":commands.len(),"offset":q.offset,"commands":page,
            "next":(next<commands.len()).then(||json!({"filter":q.filter,"offset":next,"limit":q.limit.unwrap_or(16)})),
            "instructions":"These commands include their exact params. Use engine.execute {command,params} to run them. Reuse discovered schemas; request next only if more matches are needed."})
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
        ] {
            assert!(prepare("engine.commands", &mut params).is_err());
        }
        let mut params = json!({"filter":"soundlab","offset":16,"limit":8});
        prepare("engine.commands", &mut params).unwrap();
        assert_eq!(params, json!({"filter":"soundlab"}));
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
