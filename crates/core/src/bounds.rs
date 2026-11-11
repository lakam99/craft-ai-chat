//! Bound JSON before serializing it. Count bytes without allocating a second response buffer.
use serde::Serialize;
use serde_json::Value;

pub fn value_size(value: &Value, limit: usize) -> Result<usize, String> {
    let mut stack = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if depth > 64 || nodes > 131_072 {
            return Err("AI data exceeds the JSON complexity limit".into());
        }
        let children = match value {
            Value::Array(v) => v.len(),
            Value::Object(v) => v.len(),
            _ => 0,
        };
        if children
            > 131_072usize
                .saturating_sub(nodes)
                .saturating_sub(stack.len())
        {
            return Err("AI data exceeds the JSON complexity limit".into());
        }
        match value {
            Value::Array(v) => stack.extend(v.iter().map(|v| (v, depth + 1))),
            Value::Object(v) => {
                for (key, v) in v {
                    if key.len() > limit {
                        return Err("AI data exceeds its size limit".into());
                    }
                    stack.push((v, depth + 1));
                }
            }
            Value::String(v) if v.len() > limit => {
                return Err("AI data exceeds its size limit".into());
            }
            _ => {}
        }
    }
    serialized_size(value, limit)
}
pub fn serialized_size(value: &impl Serialize, limit: usize) -> Result<usize, String> {
    struct Counter {
        size: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.size) {
                return Err(std::io::Error::other("AI data size limit"));
            }
            self.size += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { size: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| {
        "AI context or tool output exceeds its size limit. Clear the chat to continue.".to_string()
    })?;
    Ok(counter.size)
}
