// made by CasualArclamp
//! DecAero as an Android app: the desktop GUI's code (`decaero-gui`) in its
//! touch layout, started from a `NativeActivity`.
//!
//! What a phone does differently is handed over in a `Platform`:
//!
//! * the files go in the app's own folder on shared storage
//!   (`Android/data/com.casualarclamp.decaero/files`);
//! * a recording is picked with the system's file chooser;
//! * a USB radio is opened by Android, after its permission dialog, and
//!   reaches the driver as a file descriptor.
//!
//! The last two are Java's business (`android/.../MainActivity.java`); this
//! file calls it through JNI. Built by `scripts/build-android.ps1`.
//!
//! Receive only, as the desktop program.
#![cfg(target_os = "android")]

use decaero_gui::{AndroidApp, Picker, Platform};
use jni::objects::{JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use std::path::PathBuf;

/// The activity object (a global JNI reference), kept as a number so it can
/// sit in a static. `ndk_context` would give the application, which has
/// none of `MainActivity`'s methods.
static ACTIVITY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Run `f` with a JNI environment for this thread and the activity object.
///
/// Rust note: a thread Rust started is unknown to the Java machine until it
/// is attached; `attach_current_thread` does that and detaches again when
/// its guard is dropped at the end of this function.
fn java<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
    let ctx = ndk_context::android_context();
    let activity = *ACTIVITY.get().ok_or("no activity yet")?;
    // SAFETY: android-activity sets the Java machine's pointer before
    // `android_main` runs, and `ACTIVITY` is its global reference to the
    // activity, which lasts as long as the native program does.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(activity as jni::sys::jobject) };
    let result = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
}

/// Ask Android for a USB device with one of these ids: `MainActivity.openUsb`
/// shows the permission dialog if need be and waits for the answer, so this
/// must not be called on the thread that draws.
fn open_usb(ids: &[(u16, u16)]) -> Result<i32, String> {
    let flat: Vec<i32> = ids
        .iter()
        .flat_map(|(v, p)| [i32::from(*v), i32::from(*p)])
        .collect();
    let fd = java(|env, activity| {
        let array = env.new_int_array(flat.len() as i32)?;
        env.set_int_array_region(&array, 0, &flat)?;
        env.call_method(activity, "openUsb", "([I)I", &[JValue::Object(&array)])?
            .i()
    })?;
    match fd {
        fd if fd >= 0 => Ok(fd),
        -1 => Err("not plugged in (it needs a USB OTG lead, and enough power)".into()),
        -2 => Err("permission to use it was not given".into()),
        -4 => Err("no answer to the permission question".into()),
        _ => Err("Android could not open it".into()),
    }
}

/// What the activity was started with (`am start ... --es args "..."`), for
/// trying things unattended: the desktop program's command line.
fn start_args() -> Vec<String> {
    java(|env, activity| {
        let text = env
            .call_method(activity, "startArgs", "()Ljava/lang/String;", &[])?
            .l()?;
        let text: String = env.get_string(&JString::from(text))?.into();
        Ok(text)
    })
    .map(|t| t.split_whitespace().map(str::to_string).collect())
    .unwrap_or_else(|e| {
        eprintln!("start arguments not read: {e}");
        Vec::new()
    })
}

/// The system's file chooser.
struct Chooser {
    waiting: bool,
}

impl Picker for Chooser {
    fn ask(&mut self) {
        self.waiting =
            java(|env, activity| env.call_method(activity, "pickFile", "()V", &[])?.v()).is_ok();
    }

    fn poll(&mut self) -> Option<PathBuf> {
        if !self.waiting {
            return None;
        }
        let fd = java(|env, activity| env.call_method(activity, "takePickedFd", "()I", &[])?.i())
            .unwrap_or(-1);
        match fd {
            // Still choosing.
            -1 => None,
            // Closed without choosing.
            -2 => {
                self.waiting = false;
                None
            }
            // The chosen file, already open and now ours: Linux lets it be
            // opened again by this name, which is all the reader needs.
            fd => {
                self.waiting = false;
                Some(PathBuf::from(format!("/proc/self/fd/{fd}")))
            }
        }
    }
}

/// Send what the program prints (`eprintln!`) to Android's log, where
/// `adb logcat -s DecAero` shows it: a native program's stderr goes nowhere.
fn log_to_logcat() {
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;
    unsafe extern "C" {
        fn __android_log_write(prio: i32, tag: *const u8, text: *const u8) -> i32;
    }
    let mut ends = [0i32; 2];
    // SAFETY: plain POSIX calls on descriptors this function owns: a pipe is
    // made and put in place of stdout and stderr.
    unsafe {
        if libc::pipe(ends.as_mut_ptr()) != 0 {
            return;
        }
        libc::dup2(ends[1], libc::STDOUT_FILENO);
        libc::dup2(ends[1], libc::STDERR_FILENO);
        libc::close(ends[1]);
    }
    // SAFETY: `ends[0]` is the pipe's reading end, open, and nothing else has it.
    let pipe = unsafe { std::fs::File::from_raw_fd(ends[0]) };
    let _ = std::thread::Builder::new()
        .name("log".into())
        .spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut text = line.into_bytes();
                text.retain(|b| *b != 0);
                text.push(0);
                // SAFETY: both strings end in a zero byte. 4 is ANDROID_LOG_INFO.
                unsafe { __android_log_write(4, c"DecAero".as_ptr().cast(), text.as_ptr()) };
            }
        });
}

/// What the USB drivers report through the `log` crate ("PLL not locked",
/// which tuner was found), printed so it reaches Android's log with the rest.
struct DriverLog;

impl log::Log for DriverLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("{}: {}", record.target(), record.args());
    }
    fn flush(&self) {}
}

/// Where android-activity starts a native program.
///
/// Rust note: `no_mangle` keeps the function's name as written so the
/// library that loads us finds it; since edition 2024 it is written
/// `unsafe(no_mangle)`, because a clashing name would be undefined behaviour.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    log_to_logcat();
    if log::set_logger(&DriverLog).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let _ = ACTIVITY.set(app.activity_as_ptr() as usize);
    let data_dir = app
        .external_data_path()
        .or_else(|| app.internal_data_path());
    decaero_radio::set_usb_opener(Box::new(open_usb));
    let platform = Platform {
        touch: true,
        data_dir,
        picker: Some(Box::new(Chooser { waiting: false })),
        android_app: Some(app),
    };
    let args = start_args();
    eprintln!("DecAero {} {args:?}", env!("CARGO_PKG_VERSION"));
    if let Err(e) = decaero_gui::run(args, platform) {
        eprintln!("DecAero stopped: {e}");
    }
}
