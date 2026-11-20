//! Android: hand the Java VM and the app's Context to `ndk-context`.
//!
//! iroh's DNS resolver (system DNS servers) and network monitor (interfaces) ask Android
//! through Java, via `ndk-context`, which panics if nobody registered the context. JNA
//! loads this library with `dlopen`, so `JNI_OnLoad` never runs; the app therefore also
//! loads it with `System.loadLibrary` (the same library in the process) and calls this
//! once at startup, before opening the node.

use std::sync::Once;

use jni_sys::{jobject, JNIEnv, JavaVM, JNI_OK};

static INIT: Once = Once::new();

/// `TasksApplication.initAndroidContext(context)` (Kotlin `external fun`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_jurisgames_tasks_TasksApplication_initAndroidContext(env: *mut JNIEnv, _this: jobject, context: jobject) {
    INIT.call_once(|| unsafe {
        let fns = &**env;
        let (Some(get_vm), Some(new_global)) = (fns.GetJavaVM, fns.NewGlobalRef) else { return };
        let mut vm: *mut JavaVM = std::ptr::null_mut();
        if get_vm(env, &mut vm) != JNI_OK || vm.is_null() {
            return;
        }
        // A global reference: the Application object lives as long as the process.
        let global = new_global(env, context);
        if !global.is_null() {
            ndk_context::initialize_android_context(vm.cast(), global.cast());
        }
    });
}

/// Whether the context is registered (diagnostics and tests).
pub fn context_ready() -> bool {
    std::panic::catch_unwind(ndk_context::android_context).is_ok()
}
