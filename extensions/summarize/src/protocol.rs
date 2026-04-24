use std::io::Write;

use serde_json::Value;

pub fn write_result(out: &mut impl Write, id: Option<Value>, result: Value) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    });
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

pub fn write_error(out: &mut impl Write, id: Option<Value>, code: i32, message: &str) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    });
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
