//! Android owns real RFCOMM sockets; shared Rust owns bounded OPP framing/files.
use std::{path::Path, sync::Mutex};
use db2010_host::bluetooth::{OppReceiver, OppSenderFile, RECEIVE_MTU};
use jni::{JNIEnv, objects::{JByteArray, JObject, JString}, sys::{jbyteArray, jlong, jstring}};
use super::{new_string, read_string};

fn sender(handle: jlong) -> Option<&'static Mutex<OppSenderFile>> {
    if handle == 0 { None } else { Some(unsafe { &*(handle as *const Mutex<OppSenderFile>) }) }
}
fn receiver(handle: jlong) -> Option<&'static Mutex<OppReceiver>> {
    if handle == 0 { None } else { Some(unsafe { &*(handle as *const Mutex<OppReceiver>) }) }
}
fn fail(env: &mut JNIEnv<'_>, message: impl AsRef<str>) {
    let _ = env.throw_new("java/lang/IllegalStateException", message.as_ref());
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothSenderOpen<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, path: JString<'local>,
) -> jlong {
    let result = read_string(&mut env, &path).and_then(|path| OppSenderFile::new(Path::new(&path)).map_err(|e| format!("{e:#}")));
    match result { Ok(value) => Box::into_raw(Box::new(Mutex::new(value))) as jlong, Err(error) => { fail(&mut env, error); 0 } }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothSenderNext<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jbyteArray {
    let Some(sender) = sender(handle) else { fail(&mut env, "Bluetooth sender closed"); return std::ptr::null_mut(); };
    let result = sender.lock().unwrap_or_else(|e| e.into_inner()).next_packet();
    match result {
        Ok(Some(packet)) => env.byte_array_from_slice(&packet).map(|array| array.into_raw()).unwrap_or(std::ptr::null_mut()),
        Ok(None) => std::ptr::null_mut(),
        Err(error) => { fail(&mut env, format!("{error:#}")); std::ptr::null_mut() }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothSenderResponse<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong, packet: JByteArray<'local>,
) -> jstring {
    let result = (|| -> Result<(), String> {
        let sender = sender(handle).ok_or("Bluetooth sender closed")?;
        let length = env.get_array_length(&packet).map_err(|e| e.to_string())? as usize;
        if !(3..=RECEIVE_MTU).contains(&length) { return Err("Invalid Bluetooth response size".into()); }
        let bytes = env.convert_byte_array(&packet).map_err(|e| e.to_string())?;
        sender.lock().unwrap_or_else(|e| e.into_inner()).accept_response(&bytes).map_err(|e| format!("{e:#}"))
    })();
    new_string(&mut env, &result.err().unwrap_or_default())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothSenderProgress<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jstring {
    let value = sender(handle).map(|sender| {
        let sender = sender.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, total) = sender.progress();
        serde_json::json!({"bytes":bytes,"total":total,"finished":sender.finished()})
    }).unwrap_or(serde_json::Value::Null);
    new_string(&mut env, &value.to_string())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothSenderClose(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong,
) { if handle != 0 { drop(unsafe { Box::from_raw(handle as *mut Mutex<OppSenderFile>) }); } }

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothReceiverOpen<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, directory: JString<'local>,
) -> jlong {
    let result = read_string(&mut env, &directory).and_then(|path| OppReceiver::new(Path::new(&path)).map_err(|e| format!("{e:#}")));
    match result { Ok(value) => Box::into_raw(Box::new(Mutex::new(value))) as jlong, Err(error) => { fail(&mut env, error); 0 } }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothReceiverPacket<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong, packet: JByteArray<'local>,
) -> jbyteArray {
    let result = (|| -> Result<Vec<u8>, String> {
        let receiver = receiver(handle).ok_or("Bluetooth receiver closed")?;
        let length = env.get_array_length(&packet).map_err(|e| e.to_string())? as usize;
        if !(3..=RECEIVE_MTU).contains(&length) { return Err("Invalid Bluetooth request size".into()); }
        let bytes = env.convert_byte_array(&packet).map_err(|e| e.to_string())?;
        let mut receiver = receiver.lock().unwrap_or_else(|e| e.into_inner());
        Ok(receiver.handle_packet(&bytes).unwrap_or_else(|_| receiver.reject_packet()))
    })();
    match result {
        Ok(bytes) => env.byte_array_from_slice(&bytes).map(|array| array.into_raw()).unwrap_or(std::ptr::null_mut()),
        Err(error) => { fail(&mut env, error); std::ptr::null_mut() }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothReceiverTake<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jstring {
    let value = receiver(handle).and_then(|receiver| receiver.lock().unwrap_or_else(|e| e.into_inner()).take_received())
        .and_then(|file| serde_json::to_value(file).ok()).unwrap_or(serde_json::Value::Null);
    new_string(&mut env, &value.to_string())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeBluetoothReceiverClose(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong,
) { if handle != 0 { drop(unsafe { Box::from_raw(handle as *mut Mutex<OppReceiver>) }); } }
