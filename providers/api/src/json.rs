//! JSON word accounting and bounded serialization.
use serde::Serialize;
use serde_json::Value;
use std::io;

pub fn words(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}

/// Count keys and values before JSON escaping adds letters to control characters.
pub fn json_words(value: &Value) -> usize {
    match value {
        Value::String(text) => words(text),
        Value::Array(items) => items.iter().map(json_words).sum(),
        Value::Object(fields) => fields.iter().map(|(key, v)| words(key) + json_words(v)).sum(),
        Value::Number(n) => words(&n.to_string()),
        Value::Bool(false) => 5,
        Value::Bool(true) | Value::Null => 4,
    }
}

struct Count(usize);

impl io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.checked_sub(bytes.len()).ok_or(io::ErrorKind::WriteZero)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stop at the byte limit; never allocate an encoded copy just to reject it.
pub fn fits_bytes<T: Serialize + ?Sized>(value: &T, limit: usize) -> bool {
    serde_json::to_writer(&mut Count(limit), value).is_ok()
}
