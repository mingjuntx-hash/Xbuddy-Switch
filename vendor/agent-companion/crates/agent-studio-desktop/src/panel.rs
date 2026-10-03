//! macOS-only: retarget the rail window to a non-activating panel.
//!
//! Clicking any window of an inactive application activates that application,
//! which brings all of its visible windows to the front - with the rail and
//! its host in one process, clicking the rail dragged the host window forward.
//! `NSWindowStyleMaskNonactivatingPanel` is the only official mechanism that
//! suppresses that activation, and AppKit applies it only to `NSPanel` (or a
//! subclass). tao 0.35.3 has no panel API - its window class is the internal
//! `TaoWindow: NSWindow` - so the window's class is swapped after creation,
//! the same approach the tauri-nspanel crate uses.
//!
//! Hard dependency on tao 0.35.3 internals (`platform_impl/macos/window.rs`):
//! the rail's class is named `TaoWindow` and carries one ivar, `focusable`
//! (`objc2::runtime::Bool`, encoding "B"), placed after `NSWindow`'s ivars.
//! `convert` checks the class name, the instance size and the `focusable`
//! offset before swapping, so a tao upgrade that changes any of them leaves
//! the rail a normal window instead of corrupting the object. `AgentStudioRailPanel` declares the same ivar
//! with the same type at the same offset because tao's `set_focusable` and
//! `is_focusable` read it by name through `get_mut_ivar::<Bool>("focusable")`,
//! which panics when the ivar is missing. tao's `sendEvent:` override (drag
//! by window background) is not re-added: the rail never sets that attribute.
//!
//! The rail's runtime class is usually *not* `TaoWindow` but the subclass KVO
//! generates for it (`NSKVONotifying_TaoWindow`). WebKit installs that shim
//! while its web view is attached, observing the window's `contentLayoutRect`,
//! so a naive `object_setClass` leaves a live KVO registration pointing at a
//! class the object no longer has: WebKit's later `removeObserver:` cannot
//! find it and throws `NSRangeException`, which aborts the process (verified
//! on macOS 26 with a Swift probe). `convert` therefore detaches the content
//! view before the swap - WebKit's `viewWillMoveToWindow:` removes the
//! observer while the shim is still installed - and re-attaches it after, so
//! WebKit registers again, now against `AgentStudioRailPanel` (whose layout
//! matches the shim's: same ivar, same instance size).
//!
//! Every AppKit call here is main-thread-only; `create_rail` already runs on
//! the main thread via `run_on_main_thread`.
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{ffi, sel, ClassType};
use objc2_app_kit::{NSPanel, NSWindow, NSWindowStyleMask};
use std::ffi::CStr;
use std::sync::OnceLock;

/// tao 0.35.3's window class name.
const TAO_WINDOW_CLASS: &CStr = c"TaoWindow";
/// The prefix KVO gives the subclasses it generates.
const KVO_CLASS_PREFIX: &[u8] = b"NSKVONotifying_";
/// Runtime name of the replacement class; registered once per process.
const PANEL_CLASS_NAME: &CStr = c"AgentStudioRailPanel";
/// tao's focusability ivar. Name and type are load-bearing.
const FOCUSABLE_IVAR: &CStr = c"focusable";

/// `b"NSKVONotifying_TaoWindow"` -> `Some(b"TaoWindow")`; names that are not
/// KVO subclasses -> `None`.
fn kvo_shim_base(name: &[u8]) -> Option<&[u8]> {
    name.strip_prefix(KVO_CLASS_PREFIX)
}

/// Decide from a class name and its superclass's name whether the class may be
/// swapped to [`PANEL_CLASS_NAME`]: tao's `TaoWindow`, or the shim KVO derived
/// from it. `Some(is_shim)` means "convertible"; `None` means "leave alone".
///
/// The shim only ever wraps the exact class it was generated for, so anything
/// else - a web view, a different window class, a shim over a different base -
/// is rejected and the rail stays a plain window.
fn convertible_class(name: &[u8], superclass_name: Option<&[u8]>) -> Option<bool> {
    if name == TAO_WINDOW_CLASS.to_bytes() {
        return Some(false);
    }
    (kvo_shim_base(name) == Some(TAO_WINDOW_CLASS.to_bytes())
        && superclass_name == Some(TAO_WINDOW_CLASS.to_bytes()))
    .then_some(true)
}

/// Resolve `class` to tao's `TaoWindow`, tolerating the KVO shim.
fn tao_window_class(class: &AnyClass) -> Option<(&AnyClass, bool)> {
    let superclass = class.superclass();
    let is_shim = convertible_class(
        class.name().to_bytes(),
        superclass.map(|class| class.name().to_bytes()),
    )?;
    if is_shim {
        superclass.map(|class| (class, true))
    } else {
        Some((class, false))
    }
}

/// Read the focusability flag, replicating tao's `is_focusable`.
///
/// A missing ivar answers `false`, which is the value the rail is created
/// with (`focusable(false)`) and the safe direction: the window then never
/// becomes key or main.
fn focusable(this: &AnyObject) -> Bool {
    let Some(ivar) = this.class().instance_variable(FOCUSABLE_IVAR) else {
        return Bool::NO;
    };
    // SAFETY: the ivar is declared as `Bool` before the class is registered,
    // and `this` is a live instance of that class. The ivar is
    // interior-mutable main-thread window state, like every property written
    // in `convert`.
    unsafe { ivar.load_ptr::<Bool>(this).read() }
}

/// `-canBecomeKeyWindow` must follow the `focusable` flag: a swapped window
/// that answered differently from tao's override could steal key status.
///
/// The receiver is a raw pointer because a borrowed receiver would make the
/// function pointer higher-ranked, which `ClassBuilder::add_method` cannot
/// accept.
unsafe extern "C-unwind" fn can_become_key_window(this: *mut AnyObject, _cmd: Sel) -> Bool {
    // SAFETY: AppKit always passes a valid window instance as the receiver.
    focusable(unsafe { &*this })
}

/// `-canBecomeMainWindow`, same rule as [`can_become_key_window`].
unsafe extern "C-unwind" fn can_become_main_window(this: *mut AnyObject, _cmd: Sel) -> Bool {
    // SAFETY: AppKit always passes a valid window instance as the receiver.
    focusable(unsafe { &*this })
}

/// The `AgentStudioRailPanel: NSPanel` class, registered on first use.
///
/// `define_class!` cannot express this class: it registers ivars as one
/// struct behind a single `ivars` slot, while tao looks `focusable` up by
/// name, so `ClassBuilder` is used directly (the same runtime API tao itself
/// uses). `None` means the class could not be registered, e.g. because a
/// class with that name already exists.
fn rail_panel_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        let mut builder = ClassBuilder::new(PANEL_CLASS_NAME, NSPanel::class())?;
        builder.add_ivar::<Bool>(FOCUSABLE_IVAR);
        // SAFETY: both methods take no arguments and return AppKit's `BOOL`,
        // matching the signatures they override on NSPanel/NSWindow, and both
        // only touch the ivar declared above.
        unsafe {
            builder.add_method::<AnyObject, _>(
                sel!(canBecomeKeyWindow),
                can_become_key_window as unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
            );
            builder.add_method::<AnyObject, _>(
                sel!(canBecomeMainWindow),
                can_become_main_window as unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
            );
        }
        Some(builder.register())
    })
}

/// Turn an already created tao rail window into a non-activating panel.
///
/// Returns `Err` without touching the window when the pointer is null, the
/// window is not a tao window of the expected class, or its layout does not
/// match the replacement class; the caller then keeps the plain window, which
/// still works (it only activates the application on click).
pub fn convert(ns_window: usize) -> Result<(), String> {
    if ns_window == 0 {
        return Err("悬浮栏窗口句柄为空".into());
    }
    // SAFETY: a non-null NSWindow pointer owned by tao for the lifetime of
    // the application; only the object header is read until the swap below.
    let object: &AnyObject = unsafe { &*(ns_window as *const AnyObject) };
    let current = object.class();
    let Some((tao_class, is_kvo_shim)) = tao_window_class(current) else {
        return Err(format!(
            "悬浮栏窗口类为 {}，不是 tao 的 TaoWindow，保持原状",
            current.name().to_string_lossy()
        ));
    };
    let Some(panel_class) = rail_panel_class() else {
        return Err("AgentStudioRailPanel 注册失败，保持原状".into());
    };
    // The swap keeps the object's memory, so every ivar must land on the same
    // offset with the same size in both classes. `tao_class` owns the layout
    // the object was allocated for; the shim, if present, is a KVO subclass
    // of it and must not have grown either.
    if current.instance_size() != panel_class.instance_size()
        || tao_class.instance_size() != panel_class.instance_size()
    {
        return Err(format!(
            "悬浮栏窗口与 AgentStudioRailPanel 的实例大小不一致（{} vs {} 字节），保持原状",
            current.instance_size(),
            panel_class.instance_size()
        ));
    }
    // Look the ivar up before the swap: after it, nothing below can fail.
    let focusable_ivar = panel_class
        .instance_variable(FOCUSABLE_IVAR)
        .ok_or("AgentStudioRailPanel 缺少 focusable ivar")?;
    // Equal instance sizes alone leave room for a smaller ivar hidden in the
    // tail padding, so require tao's own `focusable` to sit at the same offset
    // as ours: that is what makes tao's `set_focusable`/`is_focusable` read the
    // same bytes after the swap.
    if tao_class
        .instance_variable(FOCUSABLE_IVAR)
        .map(|ivar| ivar.offset())
        != Some(focusable_ivar.offset())
    {
        return Err(
            "AgentStudioRailPanel 的 focusable ivar 与 TaoWindow 不同名或不同偏移，保持原状".into(),
        );
    }
    // SAFETY: the object's class stays a window class and the pointer keeps
    // pointing at the same live instance; tao owns the only wrapper.
    let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
    // A KVO shim means KVO swizzled this class for WebKit's observation of the
    // window - WebKit registers `contentLayoutRect` as soon as the window hosts
    // its web view, and the shim stays behind afterwards even once the
    // observer is gone. Swapping the class breaks that bookkeeping either way:
    // a later `removeObserver:` throws `NSRangeException` and aborts. Detaching
    // the content view makes WebKit unregister while the class it registered
    // against is still installed; re-attaching below registers against the
    // panel class.
    //
    // Nothing may fire a window notification while the content view is gone:
    // tao's delegate answers `windowDidResize` by unwrapping the window's
    // content view, and a panic across that call aborts the process. The
    // style mask below is therefore applied after the re-attach (setting it
    // here did abort the packaged app's smoke test).
    let content_view = is_kvo_shim.then(|| window.contentView()).flatten();
    if let Some(content_view) = &content_view {
        content_view.removeFromSuperview();
    }
    // SAFETY: AgentStudioRailPanel has the same instance layout as TaoWindow
    // (NSWindow plus one `Bool` ivar), the instance sizes match, and its only
    // overrides read an ivar both classes declare identically. The raw runtime
    // call is used instead of `AnyObject::set_class` so that a future layout
    // change stays an `Err` above rather than an assertion.
    unsafe { ffi::object_setClass(ns_window as *mut AnyObject, panel_class) };
    // tao created the rail with `.focusable(false)`; re-assert it as this
    // class's ivar so `canBecomeKeyWindow`/`canBecomeMainWindow` agree.
    // SAFETY: the ivar is declared as `Bool` on the class the object belongs
    // to from here on; the write targets main-thread-only window state.
    unsafe { focusable_ivar.load_ptr::<Bool>(object).write(Bool::NO) };
    if let Some(content_view) = &content_view {
        window.setContentView(Some(content_view));
    }
    // Append the non-activating bit - the only AppKit mechanism that stops a
    // click from activating the application. The rest of the mask (tao's
    // borderless/resizable combination) has to survive, hence `|=`.
    window.setStyleMask(window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    // A panel built by AppKit hides itself when the application deactivates;
    // the rail must stay visible while another application is in front.
    window.setHidesOnDeactivate(false);
    // Clicks on the rail's controls must not make it key, matching
    // `focusable = false`.
    // SAFETY: the object is an NSPanel (subclass) instance after the swap.
    let panel: &NSPanel = unsafe { &*(ns_window as *const NSPanel) };
    panel.setBecomesKeyOnlyIfNeeded(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::encode::Encode;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;

    #[test]
    fn conversion_is_declined_for_foreign_windows() {
        // Any object whose class is not tao's TaoWindow must be left alone:
        // the caller keeps a working, if activating, window.
        let object = NSObject::new();
        let pointer = Retained::as_ptr(&object) as usize;
        let error = convert(pointer).unwrap_err();
        assert!(error.contains("TaoWindow"), "{error}");
    }

    #[test]
    fn conversion_is_declined_for_a_null_pointer() {
        assert!(convert(0).is_err());
    }

    #[test]
    fn only_taos_window_and_its_kvo_shim_are_convertible() {
        // The rail's real class on a running app: tao's window wrapped by the
        // KVO shim WebKit installs for `contentLayoutRect`.
        assert_eq!(
            convertible_class(b"NSKVONotifying_TaoWindow", Some(b"TaoWindow")),
            Some(true)
        );
        assert_eq!(
            convertible_class(b"TaoWindow", Some(b"NSWindow")),
            Some(false)
        );
        // A shim over any other base, a shim whose base is itself a shim, and
        // unrelated classes must all stay untouched.
        assert_eq!(
            convertible_class(b"NSKVONotifying_NSWindow", Some(b"NSWindow")),
            None
        );
        assert_eq!(
            convertible_class(
                b"NSKVONotifying_TaoWindow",
                Some(b"NSKVONotifying_NSWindow")
            ),
            None
        );
        assert_eq!(
            convertible_class(b"NSKVONotifying_TaoWindow", Some(b"NSWindow")),
            None
        );
        assert_eq!(convertible_class(b"NSWindow", Some(b"NSResponder")), None);
        // A name that merely contains the shim prefix is not a shim.
        assert_eq!(
            convertible_class(b"NotNSKVONotifying_TaoWindow", Some(b"TaoWindow")),
            None
        );
    }

    #[test]
    fn the_replacement_class_is_an_nspanel_with_taos_ivar_and_overrides() {
        // Registering the class also verifies - in debug builds - that both
        // overridden signatures match the ones inherited from NSWindow, and
        // that the ivar can be added.
        let class = rail_panel_class().unwrap();
        assert_eq!(class.superclass().unwrap().name(), NSPanel::class().name());
        let ivar = class.instance_variable(FOCUSABLE_IVAR).unwrap();
        // The encoding must be tao's `Bool` encoding on every architecture.
        let encoding = ivar.type_encoding();
        assert!(
            encoding
                .to_str()
                .ok()
                .is_some_and(|encoding| Bool::ENCODING.equivalent_to_str(encoding)),
            "focusable ivar encoding is {}",
            encoding.to_string_lossy(),
        );
        for selector in [sel!(canBecomeKeyWindow), sel!(canBecomeMainWindow)] {
            assert_ne!(
                class.instance_method(selector),
                NSPanel::class().instance_method(selector),
                "{selector} is not overridden",
            );
        }
    }
}
