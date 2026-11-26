use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use e830::devices::{runtime::Frame, selection::FirmwareSelection};
use k750_native::worker::{validate_nand_image, FirmwarePaths, Worker, WorkerEvent, HostOptions, LcdFrame};
use jni::objects::{JObject, JString};
use jni::sys::{jboolean, jbyteArray, jint, jlong, jstring};
use jni::JNIEnv;
mod bluetooth_bridge;
mod card_bridge;

struct Session {
    worker: Worker,
    frame: Option<GuestFrame>,
    status: String,
    finished: bool,
    failed: bool,
}

enum GuestFrame { Rgb(Frame), Rgb565(LcdFrame) }
static HOST: OnceLock<Mutex<Option<HostOptions>>> = OnceLock::new();

fn path(value: String) -> PathBuf {
    PathBuf::from(value)
}

fn firmware_paths(
    cla: String,
    eeprom: String,
    tfs: String,
    onenand: String,
) -> Result<FirmwarePaths, String> {
    if cla.is_empty() || eeprom.is_empty() {
        return Err("Select both the .cla firmware and matching .nsf EEPROM.".into());
    }
    Ok(FirmwarePaths {
        cla: path(cla),
        eeprom: path(eeprom),
        tfs: (!tfs.is_empty()).then(|| path(tfs)),
        onenand: (!onenand.is_empty()).then(|| path(onenand)),
    })
}

fn read_string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
    env.get_string(value)
        .map(|value| value.into())
        .map_err(|error| error.to_string())
}

fn new_string(env: &mut JNIEnv<'_>, value: &str) -> jstring {
    env.new_string(value)
        .map(|value| value.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn start_selection(selection: FirmwareSelection, workspace: Option<PathBuf>, card: Option<PathBuf>) -> Result<jlong, String> {
    let registry = k750_native::registry();
    let name = registry
        .device(&selection.device)
        .map_err(|error| error.to_string())?
        .info()
        .name;
    let options=HOST.get_or_init(||Mutex::new(None)).lock().map_err(|e|e.to_string())?.clone();
    let worker = Worker::start_with_card(selection, options, workspace, card).map_err(|error| error.to_string())?;
    Ok(Box::into_raw(Box::new(Mutex::new(Session {
        worker,
        frame: None,
        status: format!("Starting {name} firmware…"),
        finished: false,
        failed: false,
    }))) as jlong)
}

fn frame_packet(frame: Frame) -> Result<Vec<u8>, String> {
    frame.validate().map_err(|error| error.to_string())?;
    let width = u32::try_from(frame.width).map_err(|error| error.to_string())?;
    let height = u32::try_from(frame.height).map_err(|error| error.to_string())?;
    let mut packet = Vec::with_capacity(8 + frame.rgb.len());
    packet.extend_from_slice(&width.to_le_bytes());
    packet.extend_from_slice(&height.to_le_bytes());
    packet.extend_from_slice(&frame.rgb);
    Ok(packet)
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeProfiles<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
) -> jstring {
    let registry = k750_native::registry();
    let profiles: Vec<_> = registry.devices().into_iter().map(|info| {
        let loader = registry.loader(info.default_loader).unwrap();
        serde_json::json!({ "info": info, "roles": loader.roles(), "selection_hint": loader.selection_hint() })
    }).collect();
    new_string(&mut env, &serde_json::to_string(&profiles).unwrap())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeCatalog<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>,
) -> jstring {
    new_string(&mut env, &serde_json::to_string(k750_native::catalog::MANUFACTURERS).unwrap())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeValidateSelection<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    json: JString<'local>,
) -> jstring {
    let result = (|| -> Result<(), String> {
        let selection: FirmwareSelection = serde_json::from_str(&read_string(&mut env, &json)?)
            .map_err(|error| error.to_string())?;
        k750_native::registry()
            .load(
                &selection.device,
                selection.loader.as_deref(),
                &selection.files,
            )
            .map_err(|error| format!("{error:#}"))?;
        Ok(())
    })();
    new_string(&mut env, &result.err().unwrap_or_default())
}

fn memory_selection(selection: &FirmwareSelection) -> Result<&PathBuf, String> {
    if selection.device != "k750i" {
        return Err("This memory format is only supported for K750i.".into());
    }
    selection.files.companions.get("fs").ok_or_else(|| "Select the matching K750i FS.".into())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeCreateInternalMemory<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, json: JString<'local>, workspace: JString<'local>,
) -> jstring {
    let result = (|| -> Result<(), String> {
        let selection: FirmwareSelection = serde_json::from_str(&read_string(&mut env, &json)?)
            .map_err(|error| error.to_string())?;
        db2010_host::prepare::create_internal_memory(
            &selection.files.image, memory_selection(&selection)?, &path(read_string(&mut env, &workspace)?),
        ).map_err(|error| format!("{error:#}"))
    })();
    new_string(&mut env, &result.err().unwrap_or_default())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeTransferInternalMemory<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    json: JString<'local>,
    data_directory: JString<'local>,
    workspace: JString<'local>,
    file: JString<'local>,
    importing: jboolean,
) -> jstring {
    let result = (|| -> Result<(), String> {
        let selection: FirmwareSelection = serde_json::from_str(&read_string(&mut env, &json)?)
            .map_err(|error| error.to_string())?;
        let fs = memory_selection(&selection)?;
        let workspace = read_string(&mut env, &workspace)?;
        let workspace = if workspace.is_empty() {
            db2010_host::prepare::workspace(
                &selection.files.image, fs, &path(read_string(&mut env, &data_directory)?),
            ).map_err(|error| format!("{error:#}"))?
        } else {
            path(workspace)
        };
        let file = path(read_string(&mut env, &file)?);
        if importing != 0 {
            db2010_host::qmp::import_internal_memory(&workspace, &file)
        } else {
            db2010_host::qmp::export_internal_memory(&workspace, &file)
        }.map_err(|error| format!("{error:#}"))
    })();
    new_string(&mut env, &result.err().unwrap_or_default())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeStartSelection<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    json: JString<'local>,
    workspace: JString<'local>,
    card: JString<'local>,
) -> jlong {
    let result = (|| -> Result<jlong, String> {
        let selection = serde_json::from_str(&read_string(&mut env, &json)?)
            .map_err(|error| error.to_string())?;
        let workspace = read_string(&mut env, &workspace)?;
        let card = read_string(&mut env, &card)?;
        start_selection(selection, (!workspace.is_empty()).then(|| path(workspace)),
            (!card.is_empty()).then(|| path(card)))
    })();
    match result {
        Ok(handle) => handle,
        Err(error) => {
            let _ = env.throw_new("java/lang/IllegalStateException", error);
            0
        }
    }
}

fn session(handle: jlong) -> Option<&'static Mutex<Session>> {
    if handle == 0 {
        return None;
    }
    Some(unsafe { &*(handle as *const Mutex<Session>) })
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeTakeAudio<'local>(
    env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jbyteArray {
    let Some(session) = session(handle) else { return std::ptr::null_mut(); };
    let session = session.lock().unwrap_or_else(|e| e.into_inner());
    let Some(packet) = session.worker.audio.take() else { return std::ptr::null_mut(); };
    env.byte_array_from_slice(&packet.wire()).map(|bytes| bytes.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeDiagnostics<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jstring {
    let value = session(handle).map(|session| {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        serde_json::to_string(&session.worker.diagnostics.snapshot()).unwrap_or_else(|_| "{}".into())
    }).unwrap_or_else(|| "{}".into());
    new_string(&mut env, &value)
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeRecordPresentations(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong, count: jint,
) {
    if let Some(session) = session(handle) {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        session.worker.diagnostics.record_presentations(count.clamp(0, 240) as u64);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeRecordRender(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong, nanos: jlong,
) {
    if let Some(session) = session(handle) {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        if (0..=1_000_000_000).contains(&nanos) { session.worker.diagnostics.record_render(std::time::Duration::from_nanos(nanos as u64)); }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeSetDeveloperMode(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong, enabled: jboolean,
) {
    if let Some(session) = session(handle) {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        session.worker.diagnostics.set_developer_mode(enabled != 0);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeLog<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
    level: JString<'local>, component: JString<'local>, message: JString<'local>,
) {
    let Ok(level) = read_string(&mut env, &level) else { return; };
    let Ok(component) = read_string(&mut env, &component) else { return; };
    let Ok(message) = read_string(&mut env, &message) else { return; };
    if let Some(session) = session(handle) {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        session.worker.diagnostics.log(&level, &component, &message);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeFiles<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong,
) -> jstring {
    let value = session(handle).map(|session| {
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        serde_json::to_string(&session.worker.files.snapshot()).unwrap_or_else(|_| "{}".into())
    }).unwrap_or_else(|| "{}".into());
    new_string(&mut env, &value)
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeFileRequest<'local>(
    mut env: JNIEnv<'local>, _this: JObject<'local>, handle: jlong, request: JString<'local>,
) -> jstring {
    let result = (|| -> Result<u64, String> {
        let request: db2010_host::storage::FileRequest = serde_json::from_str(&read_string(&mut env, &request)?)
            .map_err(|error| error.to_string())?;
        let session = session(handle).ok_or_else(|| "The phone is not running.".to_string())?;
        let session = session.lock().unwrap_or_else(|error| error.into_inner());
        session.worker.files.request(request).map_err(|error| error.to_string())
    })();
    let response = match result { Ok(id) => serde_json::json!({"id":id}), Err(error) => serde_json::json!({"error":error}) };
    new_string(&mut env, &response.to_string())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeValidate<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    cla: JString<'local>,
    eeprom: JString<'local>,
    tfs: JString<'local>,
    onenand: JString<'local>,
) -> jstring {
    let result = (|| {
        let paths = firmware_paths(
            read_string(&mut env, &cla)?,
            read_string(&mut env, &eeprom)?,
            read_string(&mut env, &tfs)?,
            read_string(&mut env, &onenand)?,
        )?;
        if let Some(nand) = paths.onenand_path() {
            validate_nand_image(&nand)?;
        }
        paths.load().map(|_| ()).map_err(|error| error.to_string())
    })();
    new_string(&mut env, &result.err().unwrap_or_default())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    cla: JString<'local>,
    eeprom: JString<'local>,
    tfs: JString<'local>,
    onenand: JString<'local>,
) -> jlong {
    let result = (|| {
        let paths = firmware_paths(
            read_string(&mut env, &cla)?,
            read_string(&mut env, &eeprom)?,
            read_string(&mut env, &tfs)?,
            read_string(&mut env, &onenand)?,
        )?;
        start_selection(paths.into(), None, None)
    })();
    match result {
        Ok(handle) => handle,
        Err(error) => {
            let _ = env.throw_new("java/lang/IllegalStateException", error);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativePollStatus<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    handle: jlong,
) -> jstring {
    let Some(session) = session(handle) else {
        return new_string(&mut env, "Emulator is not running.");
    };
    let mut session = session.lock().unwrap_or_else(|error| error.into_inner());
    poll_worker(&mut session);
    new_string(&mut env, &session.status)
}

fn poll_worker(session: &mut Session) {
    loop {
        match session.worker.events.try_recv() {
            Ok(WorkerEvent::Frame(frame)) => {
                session.frame = Some(GuestFrame::Rgb(frame));
                if !session.finished {
                    session.status = "Running. Displaying the guest firmware LCD.".into();
                }
            }
            Ok(WorkerEvent::Lcd(frame)) => session.frame = Some(GuestFrame::Rgb565(frame)),
            Ok(WorkerEvent::Status(status)) => if !session.finished {session.status=status;},
            Ok(WorkerEvent::InputApplied(_)) => {}
            Ok(WorkerEvent::Finished(status)) => {
                session.failed = status.is_err();
                session.status = status.unwrap_or_else(|error| error);
                session.finished = true;
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !session.finished {
                    session.status = "Emulator worker stopped unexpectedly.".into();
                    session.failed = true;
                    session.finished = true;
                }
                break;
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeTakeFrame<'local>(
    env: JNIEnv<'local>,
    _this: JObject<'local>,
    handle: jlong,
) -> jbyteArray {
    let Some(session) = session(handle) else {
        return std::ptr::null_mut();
    };
    let mut session = session.lock().unwrap_or_else(|error| error.into_inner());
    let Some(frame) = session.frame.take() else {
        return std::ptr::null_mut();
    };
    let packet = match frame {
        GuestFrame::Rgb(frame) => match frame_packet(frame) {
            Ok(packet)=>packet, Err(_)=>return std::ptr::null_mut(),
        },
        GuestFrame::Rgb565(frame) => {
            let mut bytes=Vec::with_capacity(8+frame.rgb565_be.len());
            bytes.extend_from_slice(&(frame.width as u32).to_le_bytes());
            bytes.extend_from_slice(&(frame.height as u32).to_le_bytes());
            // Android RGB_565 bitmaps consume native little-endian words.
            for pair in frame.rgb565_be.chunks_exact(2) {bytes.extend_from_slice(&[pair[1],pair[0]]);}
            bytes
        }
    };
    env.byte_array_from_slice(&packet)
        .map(|array| array.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeConfigure<'local>(
    mut env:JNIEnv<'local>, _this:JObject<'local>, engine:JString<'local>, data:JString<'local>, resume:jboolean,
) {
    let result=(|| -> Result<(),String> {
        let options=HostOptions {engine:read_string(&mut env,&engine)?.into(),
            data_dir:read_string(&mut env,&data)?.into(),resume:resume!=0};
        *HOST.get_or_init(||Mutex::new(None)).lock().map_err(|e|e.to_string())?=Some(options);
        Ok(())
    })();
    if let Err(error)=result {let _=env.throw_new("java/lang/IllegalStateException",error);}
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeSetKeys(
    _env:JNIEnv<'_>, _this:JObject<'_>, handle:jlong, mask:jint,
) {
    if let Some(session)=session(handle) {
        if let Ok(session)=session.lock() {session.worker.set_phone_keys(mask as u32);}
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeKeyBindings<'local>(
    mut env:JNIEnv<'local>, _this:JObject<'local>, device:JString<'local>,
) -> jstring {
    let device=read_string(&mut env,&device).unwrap_or_default();
    new_string(&mut env,&serde_json::to_string(&k750_native::keys::bindings(&device)).unwrap())
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeIsRunning(
    _env: JNIEnv<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jboolean {
    session(handle)
        .and_then(|session| session.lock().ok().map(|session| !session.finished))
        .unwrap_or(false) as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeSetPaused(
    _env: JNIEnv<'_>, _this: JObject<'_>, handle: jlong, paused: jboolean,
) {
    if let Some(session) = session(handle) {
        if let Ok(session) = session.lock() { session.worker.set_paused(paused != 0); }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeStop<'local>(
    mut env: JNIEnv<'local>,
    _this: JObject<'local>,
    handle: jlong,
) -> jstring {
    let Some(session) = session(handle) else { return new_string(&mut env, ""); };
    let mut session = session.lock().unwrap_or_else(|error| error.into_inner());
    session.worker.stop();
    poll_worker(&mut session);
    new_string(&mut env, if session.failed { &session.status } else { "" })
}

#[no_mangle]
pub extern "system" fn Java_com_samsung_e830_NativeBridge_nativeDestroy(
    _env: JNIEnv<'_>,
    _this: JObject<'_>,
    handle: jlong,
) {
    if handle != 0 {
        unsafe {
            drop(Box::from_raw(handle as *mut Mutex<Session>));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_packet_carries_dimensions_and_rejects_malformed_rgb() {
        let packet = frame_packet(Frame {
            width: 128,
            height: 64,
            rgb: vec![0xff; 128 * 64 * 3],
            revision: 1,
        })
        .unwrap();
        assert_eq!(&packet[..8], &[128, 0, 0, 0, 64, 0, 0, 0]);
        assert_eq!(packet.len(), 8 + 128 * 64 * 3);
        assert!(frame_packet(Frame {
            width: 176,
            height: 220,
            rgb: vec![0; 3],
            revision: 0
        })
        .is_err());
    }
}
