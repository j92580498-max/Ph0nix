use std::path::PathBuf;
use db2010_host::storage::{CardImage, FileRequest};
use jni::{JNIEnv, objects::{JObject, JString}, sys::jstring};
use serde_json::{json, Value};
use super::{new_string, read_string};

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeCardRequest<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, request: JString<'local>,
) -> jstring {
    let result = (|| -> Result<Value, String> {
        let mut request: Value = serde_json::from_str(&read_string(&mut env, &request)?).map_err(|e| e.to_string())?;
        let include_snapshot = request.as_object_mut().ok_or("Invalid card request")?
            .remove("include_snapshot").and_then(|value| value.as_bool()).unwrap_or(true);
        let image = PathBuf::from(request["image"].as_str().ok_or("Choose a card image")?);
        let operation = request["operation"].as_str().unwrap_or("info").to_owned();
        let card = if operation == "create" {
            CardImage::create(&image, request["size_mib"].as_u64().unwrap_or(64))
        } else { CardImage::open(&image) }.map_err(|e| format!("{e:#}"))?;
        let mut path = Vec::new();
        if !matches!(operation.as_str(), "info" | "create") {
            request.as_object_mut().ok_or("Invalid card request")?.remove("image");
            let action: FileRequest = serde_json::from_value(request).map_err(|e| e.to_string())?;
            let result = match action {
                FileRequest::List { path: value } => { path = value; Ok(()) },
                FileRequest::Import { path: value, host, replace, move_source } => {
                    let result = card.import_file(&value, &host, replace, move_source);
                    path = value[..value.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::Export { path: value, host, move_source } => {
                    let result = card.export_file(&value, &host, move_source);
                    path = value[..value.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::Copy { source, destination, replace, move_source } => {
                    let result = card.copy(&source, &destination, replace, move_source);
                    path = destination[..destination.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::MakeDirectory { path: value } => {
                    let result = card.make_directory(&value); path = value[..value.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::Delete { path: value } => {
                    let result = card.delete(&value); path = value[..value.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::DeleteVerified { path: value, bytes, sha256 } => {
                    let result = card.delete_verified(&value, bytes, &sha256);
                    path = value[..value.len().saturating_sub(1)].to_vec(); result
                },
                FileRequest::ImportInternal { .. } => return Err("Start K750i to import to its internal storage".into()),
                FileRequest::Disconnect => Ok(()),
            };
            result.map_err(|e| format!("{e:#}"))?;
        }
        if !include_snapshot {
            if operation == "info" {
                return Ok(json!({"info":card.info().map_err(|e| format!("{e:#}"))?}));
            }
            return Ok(json!({}));
        }
        let entries = card.list(&path).map_err(|e| format!("{e:#}"))?;
        let info = card.info().map_err(|e| format!("{e:#}"))?;
        Ok(json!({"info":info,"snapshot":{"attached":true,"connected":true,"busy":false,"path":path,"entries":entries,
            "notice":"Image files. Choose this image before starting K750i to use it in the phone.","job":null}}))
    })();
    let response = match result { Ok(value) => value, Err(error) => json!({"error":error}) };
    new_string(&mut env, &response.to_string())
}
