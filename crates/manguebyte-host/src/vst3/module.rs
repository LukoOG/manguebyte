//! Loading a VST 3 module and asking its factory what it contains.

// The bindings give enumerations a different integer type on Windows than
// elsewhere, so a cast that is needed on one platform is redundant on the
// other.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{OsString, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo, kResultOk,
};
use vst3::{ComPtr, ComWrapper, Interface};

use super::objects::{HostApplication, context_ptr};
use super::text::{narrow_text, wide_text};
use super::{ClassId, MAX_CLASSES, Vst3Error, Vst3Plugin, bundle};

/// The name of the symbol every VST 3 module exports.
const FACTORY_SYMBOL: &[u8] = b"GetPluginFactory\0";

/// The category of a class that is an audio plugin.
pub(crate) const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// The name the plugins of a module are told their host has, unless the
/// caller gives another.
pub(crate) const HOST_NAME: &str = "Manguebyte";

/// What a module's factory says about whoever made it.
///
/// Every string is valid text whatever the module supplied; a field it
/// left out is empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FactoryInfo {
    /// Who made the module.
    pub vendor: String,
    /// Their web page.
    pub url: String,
    /// Where to write to them.
    pub email: String,
    /// The factory's flags, as VST 3 defines them (`PFactoryInfo::flags`).
    pub flags: i32,
}

/// What a module's factory says about one of its classes.
///
/// Read from the richest description the factory offers: the UTF-16 one of
/// `IPluginFactory3` if it has it, then the one of `IPluginFactory2`, then
/// the basic one. Every string is valid text whatever the module supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassInfo {
    /// The class identifier: what [`Vst3Module::create`] takes and what
    /// sessions store.
    pub id: ClassId,
    /// The name shown to people.
    pub name: String,
    /// What kind of class it is. An audio plugin is `"Audio Module Class"`;
    /// its controller, when it has a separate one, is listed as well, as
    /// `"Component Controller Class"`.
    pub category: String,
    /// How a host should file the plugin, as VST 3's words: `"Fx"`,
    /// `"Instrument"`, `"Synth"`, `"Delay"` and so on. Empty if the factory
    /// does not say.
    pub sub_categories: Vec<String>,
    /// Who makes it. Empty if the factory does not say per class.
    pub vendor: String,
    /// Its version, as text.
    pub version: String,
    /// The version of the VST 3 interfaces it was built with, as text.
    pub sdk_version: String,
    /// How many instances can exist at once, as VST 3 defines it.
    pub cardinality: i32,
    /// The class's flags (`ComponentFlags`), or zero if the factory does
    /// not say.
    pub flags: u32,
}

impl ClassInfo {
    /// Whether this class is an audio plugin, which is what can be
    /// created.
    pub fn is_audio_module(&self) -> bool {
        self.category == AUDIO_MODULE_CLASS
    }

    /// Whether the class files itself as an instrument.
    pub fn is_instrument(&self) -> bool {
        self.sub_categories.iter().any(|word| word == "Instrument")
    }
}

/// A loaded VST 3 module: a bundle or a library, with a factory that offers
/// one or more classes.
///
/// Loading calls the module's entry function once, as its system wants:
/// `InitDll` on Windows, `ModuleEntry` with the library's handle on Linux,
/// `bundleEntry` with the bundle on macOS. Every plugin created from the
/// module keeps it loaded; when the last plugin and the last copy of this
/// handle are gone, the factory is released, the matching exit function is
/// called once and the library is unloaded. Loading a file that is already
/// loaded gives another handle to the same module and calls nothing again.
/// A module that does not export an entry or exit function is loaded
/// without calling it.
///
/// The handle can be cloned and sent between threads.
///
/// Loading is `unsafe`, and it is the only step that is: a module is code,
/// and what it does once it is called is not something this crate can
/// check. See [`load`](Self::load) for what the caller vouches for.
///
/// ```
/// use manguebyte_host::vst3::{Vst3Error, Vst3Module};
///
/// // A path with nothing behind it is an error, not a crash.
/// // SAFETY: there is no file at this path, so no code is run.
/// let missing = unsafe { Vst3Module::load("no/such/plugin.vst3") };
/// assert!(matches!(missing, Err(Vst3Error::NotFound(_))));
/// ```
#[derive(Clone)]
pub struct Vst3Module {
    inner: Arc<Inner>,
    /// What this handle's caller asked to load, as the caller wrote it.
    /// Two handles to one module, loaded through two spellings of its
    /// path, each keep their own.
    path: PathBuf,
}

struct Inner {
    /// Released before the exit function is called. `None` only while
    /// being dropped.
    factory: Option<ComPtr<IPluginFactory>>,
    /// Given to the factory as its host context, if it takes one.
    _host: Option<ComWrapper<HostApplication>>,
    info: FactoryInfo,
    classes: Vec<ClassInfo>,
    /// Tells this module apart from every other one ever loaded.
    serial: u64,
    /// Called once, after the factory is released and before the library
    /// is unloaded.
    exit: Option<ExitFn>,
    /// The bundle given to `bundleEntry` on macOS, released after
    /// `bundleExit`.
    #[cfg(target_os = "macos")]
    bundle: mac::Bundle,
    /// The operating system's handle, or `None` for a factory that is part
    /// of the program itself.
    library: Option<libloading::Library>,
}

/// The exit function every system's modules have the same shape of.
type ExitFn = unsafe extern "system" fn() -> bool;

// The fields are Send and Sync by themselves, except for the bundle handle
// on macOS, for which see `mac::Bundle`. This fails to compile if a field
// is ever added for which that is not true.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<Inner>();
};

/// A module that is loaded right now.
struct Loaded {
    /// The file it was loaded from, in a form that is the same however the
    /// path was written.
    key: PathBuf,
    serial: u64,
    module: Weak<Inner>,
}

/// Every module loaded from a file and not yet unloaded.
///
/// It exists so that entry and exit functions stay balanced when the same
/// file is asked for twice: the second request shares the first one's
/// module. The lock is also held across entry, exit and unloading, so these
/// never overlap for two requests for the same file.
static REGISTRY: Mutex<Vec<Loaded>> = Mutex::new(Vec::new());
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

fn registry() -> MutexGuard<'static, Vec<Loaded>> {
    REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl Vst3Module {
    /// Loads a VST 3 module from a bundle folder or from a bare library,
    /// and reads the list of classes its factory offers.
    ///
    /// A bundle is a folder, usually named `Name.vst3`. The library for this
    /// system is found in it as VST 3 lays bundles out: in
    /// `Contents/<machine>-win` on Windows, `Contents/<machine>-linux` on
    /// Linux (with the machine as `uname -m` names it), and
    /// `Contents/MacOS` on macOS, where the bundle's `Info.plist` names it.
    /// Any other path is loaded as a library as it is: the older form of a
    /// VST 3 plugin on Windows, and what a build leaves before packaging.
    ///
    /// # Errors
    ///
    /// Each way this can go wrong has its own [`Vst3Error`]: nothing at the
    /// path, a bundle without a library for this system, a file the system
    /// cannot load as a library, a library with no `GetPluginFactory`, an
    /// entry function that refuses, and a module with no factory. In every
    /// case nothing stays loaded.
    ///
    /// # Safety
    ///
    /// Loading a library runs code from it, with the full rights of this
    /// process, and nothing this function can check beforehand makes that
    /// sound. The caller vouches for the file:
    ///
    /// - If the operating system can load the file as a shared library, the
    ///   code the library runs while it is loaded and unloaded must be
    ///   sound to run in this process.
    /// - If the library exports `GetPluginFactory`, it must be the VST 3
    ///   factory function, the module's entry and exit functions, where it
    ///   exports them, must be VST 3's for this system, and every object
    ///   reached through the factory must keep to the VST 3 interfaces it
    ///   says it implements: this host checks what they return, but it has
    ///   to call them.
    ///
    /// A path with nothing at it, and a file the system refuses to load as
    /// a library, meet these terms as they are: no code is run. A library
    /// with no `GetPluginFactory` is an error too, but it is loaded to find
    /// that out, so the first term has to hold for it.
    ///
    /// A damaged library does not meet them. The system's loader is not
    /// written to survive one: on Linux a library cut short after its
    /// headers is mapped without its length being checked, and the process
    /// dies inside the loader before this function can return an error.
    ///
    /// Only load files you would be willing to run.
    pub unsafe fn load(path: impl AsRef<Path>) -> Result<Self, Vst3Error> {
        let path = path.as_ref();
        let binary = bundle::binary_of(path)?;
        let key = std::fs::canonicalize(&binary).unwrap_or_else(|_| binary.clone());
        loop {
            let mut registry = registry();
            let Some(loaded) = registry.iter().find(|loaded| loaded.key == key) else {
                let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
                // SAFETY: passed on from the caller, who vouches for the
                // file at `path` and so for its library `binary`.
                let inner = Arc::new(unsafe { open(path, &binary, serial) }?);
                registry.push(Loaded {
                    key,
                    serial,
                    module: Arc::downgrade(&inner),
                });
                return Ok(Self {
                    inner,
                    path: path.to_owned(),
                });
            };
            if let Some(inner) = loaded.module.upgrade() {
                return Ok(Self {
                    inner,
                    path: path.to_owned(),
                });
            }
            // The last user of that module is letting go of it at this
            // moment, on another thread, and is waiting for the lock to
            // unload it. Step aside until it has.
            drop(registry);
            std::thread::yield_now();
        }
    }

    /// Wraps the factory of a plugin that is part of this program instead
    /// of a file on disk: a plugin linked in statically, one defined in a
    /// test, or what `GetPluginFactory` of a library loaded some other way
    /// returned.
    ///
    /// `factory` carries one reference, which the module takes over and
    /// releases when it is gone. No entry or exit function is called: the
    /// caller has done whatever the module needs. `path` is what
    /// [`path`](Self::path) returns. The example in the
    /// [module documentation](super) uses this.
    ///
    /// # Errors
    ///
    /// [`Vst3Error::NoFactory`] if `factory` is null.
    ///
    /// # Safety
    ///
    /// `factory` must be null, or point to a VST 3 `IPluginFactory` whose
    /// code stays loaded for the rest of the program, and every object
    /// reached through it must keep to the VST 3 interfaces it says it
    /// implements: this host checks what they return, but it has to call
    /// them.
    pub unsafe fn from_factory(factory: *mut c_void, path: &str) -> Result<Self, Vst3Error> {
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
        // SAFETY: passed on from the caller.
        let inner = unsafe { read_factory(factory.cast(), None, serial) }?;
        Ok(Self {
            inner: Arc::new(inner),
            path: PathBuf::from(path),
        })
    }

    /// The path this module was loaded from, as it was given to
    /// [`load`](Self::load) or [`from_factory`](Self::from_factory) for
    /// this handle and its clones. A file loaded twice, through two
    /// spellings of its path, is one module with two handles, and each
    /// handle reports the spelling it was loaded with.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the factory says about whoever made the module.
    pub fn factory_info(&self) -> &FactoryInfo {
        &self.inner.info
    }

    /// Every class the factory offers, as read when the module was loaded,
    /// in the factory's order: audio plugins, and the controllers of the
    /// ones that have a separate controller.
    ///
    /// At most [`MAX_CLASSES`] are read. A class whose description cannot
    /// be read, or whose identifier is zero or repeats an earlier one, is
    /// left out.
    pub fn classes(&self) -> &[ClassInfo] {
        &self.inner.classes
    }

    /// The class with this identifier, if the module offers one.
    pub fn class(&self, id: ClassId) -> Option<&ClassInfo> {
        self.inner.classes.iter().find(|class| class.id == id)
    }

    /// Creates the audio plugin of class `id`, with its controller.
    ///
    /// The calling thread becomes the plugin's main thread: the returned
    /// handle cannot leave it. The plugin is told its host is called
    /// `"Manguebyte"`.
    ///
    /// # Errors
    ///
    /// [`Vst3Error::UnknownClass`] if the module offers no such class,
    /// [`Vst3Error::NotAnAudioModule`] if the class is not an audio plugin,
    /// and otherwise an error describing what the plugin refused or got
    /// wrong. A plugin that fails part-way is terminated and released
    /// again.
    pub fn create(&self, id: ClassId) -> Result<Vst3Plugin, Vst3Error> {
        self.create_with(id, HOST_NAME)
    }

    /// Creates a plugin as [`create`](Self::create) does, telling it the
    /// name of the program hosting it.
    ///
    /// # Errors
    ///
    /// As for [`create`](Self::create).
    pub fn create_with(&self, id: ClassId, host_name: &str) -> Result<Vst3Plugin, Vst3Error> {
        let class = self.class(id).ok_or(Vst3Error::UnknownClass(id))?;
        if !class.is_audio_module() {
            return Err(Vst3Error::NotAnAudioModule(id));
        }
        Vst3Plugin::create(self.clone(), class.clone(), host_name)
    }

    /// Asks the factory for an object of class `id` through interface `I`.
    pub(crate) fn create_instance<I: Interface>(&self, id: ClassId) -> Option<ComPtr<I>> {
        let factory = self.inner.factory.as_ref()?;
        let class = id.tuid();
        let mut object: *mut c_void = std::ptr::null_mut();
        // SAFETY: the factory belongs to this module, which is loaded; the
        // two identifiers are 16 bytes each and outlive the call, and
        // `object` is a pointer this function owns.
        let result = unsafe {
            factory.createInstance(class.as_ptr(), I::IID.as_ptr().cast(), &raw mut object)
        };
        if result != kResultOk || object.is_null() || !object.is_aligned() {
            return None;
        }
        // SAFETY: the factory made an object of this class, handed out
        // through the interface `I` asked for, with one reference that is
        // now this host's.
        unsafe { ComPtr::from_raw(object.cast::<I>()) }
    }
}

impl std::fmt::Debug for Vst3Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Module")
            .field("path", &self.path)
            .field("classes", &self.inner.classes.len())
            .finish()
    }
}

impl PartialEq for Vst3Module {
    /// Two handles are equal when they stand for the same loaded module.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Vst3Module {}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut registry = registry();
        // Every plugin created from the module held a reference to this
        // value, so none is left. The factory goes first: the exit function
        // may free what it points to.
        drop(self.factory.take());
        if let Some(exit) = self.exit {
            // SAFETY: the entry function succeeded when this value was
            // made, and this is the one exit call that balances it.
            unsafe { exit() };
        }
        #[cfg(target_os = "macos")]
        self.bundle.release();
        // Unloaded while the lock is still held, so that a request for the
        // same file finds the module either whole or gone.
        drop(self.library.take());
        registry.retain(|loaded| loaded.serial != self.serial);
    }
}

/// The name to give the operating system so that it loads `binary`, that
/// file and no other.
///
/// Left to themselves the loaders do not always take a name at its word. A
/// name with no folder in it is looked up in the system's library folders
/// and not where the caller pointed, so the path is made absolute. And
/// Windows adds `.dll` to a file name that has no extension, as the
/// library inside a macOS bundle has not; a dot at the end tells it not to.
fn exact_name(binary: &Path) -> OsString {
    let absolute = std::path::absolute(binary).unwrap_or_else(|_| binary.to_owned());
    let needs_dot = cfg!(windows) && absolute.extension().is_none();
    let mut name = absolute.into_os_string();
    if needs_dot {
        name.push(".");
    }
    name
}

/// Loads `binary`, the library of the module at `path`, calls its entry
/// function and reads its factory. Called with the registry locked.
///
/// # Safety
///
/// As for [`Vst3Module::load`], whose two terms are the two obligations
/// taken on here.
unsafe fn open(path: &Path, binary: &Path, serial: u64) -> Result<Inner, Vst3Error> {
    // SAFETY: loading a library runs its initialisation routines. Per this
    // function's contract, which is the first term of `Vst3Module::load`,
    // the caller vouches that for this file they are sound to run here.
    let library = unsafe { libloading::Library::new(exact_name(binary)) }.map_err(|error| {
        Vst3Error::Load {
            path: binary.to_owned(),
            reason: error.to_string(),
        }
    })?;
    type FactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;
    // SAFETY: per the second term of `Vst3Module::load`, a symbol of this
    // name is VST 3's factory function, which has this type. It is copied
    // out of the symbol and only called while `library` is loaded.
    let get_factory = match unsafe { library.get::<FactoryFn>(FACTORY_SYMBOL) } {
        Ok(symbol) => *symbol,
        Err(_) => return Err(Vst3Error::NoEntryPoint(binary.to_owned())),
    };
    // SAFETY: as above, for the entry and exit functions of this system,
    // which are called while `library` is loaded and no later: `Inner`
    // holds the library and calls `exit` before unloading it.
    let (library, entry) = unsafe { entry::enter(library, path) }?;
    // SAFETY: the factory function of a module whose entry function has
    // succeeded, per the second term of `Vst3Module::load`. It hands over
    // one reference.
    let factory = unsafe { get_factory() };
    // SAFETY: as above, `factory` is the module's factory, and its code
    // stays loaded for as long as the value this returns exists, since that
    // value holds `library`.
    let outcome = unsafe { read_factory(factory, Some(entry), serial) };
    match outcome {
        Ok(mut inner) => {
            inner.library = Some(library);
            Ok(inner)
        }
        Err(error) => {
            // `read_factory` has called the exit function already, so the
            // entry is balanced and the library can go.
            drop(library);
            Err(error)
        }
    }
}

/// Reads a factory's information and its classes.
///
/// On failure, the exit function in `entry` is called before returning, so
/// that an entry that succeeded is always balanced.
///
/// # Safety
///
/// `factory` must be null, or a VST 3 `IPluginFactory` carrying one
/// reference for the caller, whose code stays loaded for as long as the
/// returned value exists, and whose objects keep to the VST 3 interfaces.
unsafe fn read_factory(
    factory: *mut IPluginFactory,
    entry: Option<entry::Entered>,
    serial: u64,
) -> Result<Inner, Vst3Error> {
    let exit = entry.as_ref().and_then(|entered| entered.exit);
    let fail = |error: Vst3Error| {
        if let Some(exit) = exit {
            // SAFETY: the entry function succeeded and nothing made since
            // is left; this balances it.
            unsafe { exit() };
        }
        #[cfg(target_os = "macos")]
        if let Some(entered) = &entry {
            entered.bundle.release();
        }
        error
    };
    if !factory.is_aligned() {
        return Err(fail(Vst3Error::NoFactory));
    }
    // SAFETY: null or a factory with one reference for this host, per the
    // contract. The reference is now held by the `ComPtr`.
    let Some(factory) = (unsafe { ComPtr::from_raw(factory) }) else {
        return Err(fail(Vst3Error::NoFactory));
    };

    // A factory that takes a host context is given one, as hosts do.
    let host = factory.cast::<IPluginFactory3>().map(|third| {
        let host = HostApplication::new(HOST_NAME);
        // SAFETY: the factory's own function, with a host context that is
        // kept alive by the value this returns for as long as the factory.
        unsafe { third.setHostContext(context_ptr(&host)) };
        host
    });

    let info = read_factory_info(&factory);
    let classes = read_classes(&factory);
    Ok(Inner {
        factory: Some(factory),
        _host: host,
        info,
        classes,
        serial,
        exit,
        #[cfg(target_os = "macos")]
        bundle: entry.map_or_else(mac::Bundle::none, |entered| entered.bundle),
        library: None,
    })
}

fn read_factory_info(factory: &ComPtr<IPluginFactory>) -> FactoryInfo {
    // SAFETY: every field is a number or an array of bytes, for which all
    // zeros is a value.
    let mut raw: PFactoryInfo = unsafe { std::mem::zeroed() };
    // SAFETY: the factory's own function, with a struct this function owns.
    if unsafe { factory.getFactoryInfo(&raw mut raw) } != kResultOk {
        return FactoryInfo::default();
    }
    FactoryInfo {
        vendor: narrow_text(&raw.vendor),
        url: narrow_text(&raw.url),
        email: narrow_text(&raw.email),
        flags: raw.flags,
    }
}

fn read_classes(factory: &ComPtr<IPluginFactory>) -> Vec<ClassInfo> {
    let second = factory.cast::<IPluginFactory2>();
    let third = factory.cast::<IPluginFactory3>();
    // SAFETY: the factory's own function.
    let count = usize::try_from(unsafe { factory.countClasses() })
        .unwrap_or(0)
        .min(MAX_CLASSES);
    let mut classes: Vec<ClassInfo> = Vec::new();
    for index in 0..count as i32 {
        // SAFETY: every field is a number or an array of bytes, for which
        // all zeros is a value.
        let mut basic: PClassInfo = unsafe { std::mem::zeroed() };
        // SAFETY: the factory's own function, with an index below the
        // count it reported and a struct this function owns.
        if unsafe { factory.getClassInfo(index, &raw mut basic) } != kResultOk {
            continue;
        }
        let mut class = ClassInfo {
            id: ClassId::from_tuid(&basic.cid),
            name: narrow_text(&basic.name),
            category: narrow_text(&basic.category),
            sub_categories: Vec::new(),
            vendor: String::new(),
            version: String::new(),
            sdk_version: String::new(),
            cardinality: basic.cardinality,
            flags: 0,
        };
        if let Some(second) = &second {
            // SAFETY: as above.
            let mut detailed: PClassInfo2 = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            if unsafe { second.getClassInfo2(index, &raw mut detailed) } == kResultOk
                && ClassId::from_tuid(&detailed.cid) == class.id
            {
                class.sub_categories = words(&narrow_text(&detailed.subCategories));
                class.vendor = narrow_text(&detailed.vendor);
                class.version = narrow_text(&detailed.version);
                class.sdk_version = narrow_text(&detailed.sdkVersion);
                class.flags = detailed.classFlags;
            }
        }
        if let Some(third) = &third {
            // SAFETY: as above; UTF-16 fields are numbers too.
            let mut unicode: PClassInfoW = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            if unsafe { third.getClassInfoUnicode(index, &raw mut unicode) } == kResultOk
                && ClassId::from_tuid(&unicode.cid) == class.id
            {
                class.name = wide_text(&unicode.name);
                class.sub_categories = words(&narrow_text(&unicode.subCategories));
                class.vendor = wide_text(&unicode.vendor);
                class.version = wide_text(&unicode.version);
                class.sdk_version = wide_text(&unicode.sdkVersion);
                class.flags = unicode.classFlags;
            }
        }
        let usable = !class.id.is_zero() && !classes.iter().any(|other| other.id == class.id);
        if usable {
            classes.push(class);
        }
    }
    classes
}

/// VST 3's sub-categories, which are words joined by `|`.
fn words(text: &str) -> Vec<String> {
    text.split('|')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The entry and exit functions, which differ from system to system.
mod entry {
    use std::path::Path;

    use super::{ExitFn, Vst3Error};

    /// What a successful entry leaves to be undone.
    pub(super) struct Entered {
        /// The exit function, if the module has one.
        pub(super) exit: Option<ExitFn>,
        /// The bundle that was given to `bundleEntry`.
        #[cfg(target_os = "macos")]
        pub(super) bundle: super::mac::Bundle,
    }

    /// Calls the module's entry function, if it exports one, and finds its
    /// exit function.
    ///
    /// # Safety
    ///
    /// `library` must be a VST 3 module whose entry and exit functions,
    /// where it exports them, are VST 3's for this system. The exit function
    /// must be called, if at all, while `library` is still loaded.
    #[cfg(windows)]
    pub(super) unsafe fn enter(
        library: libloading::Library,
        _path: &Path,
    ) -> Result<(libloading::Library, Entered), Vst3Error> {
        type EntryFn = unsafe extern "system" fn() -> bool;
        // SAFETY: per the contract, these symbols are VST 3's entry and exit
        // functions for Windows, which have these types.
        let (entry, exit) = unsafe {
            (
                library
                    .get::<EntryFn>(b"InitDll\0")
                    .ok()
                    .map(|symbol| *symbol),
                library
                    .get::<ExitFn>(b"ExitDll\0")
                    .ok()
                    .map(|symbol| *symbol),
            )
        };
        if let Some(entry) = entry {
            // SAFETY: the module's own entry function, called once, first.
            if !unsafe { entry() } {
                return Err(Vst3Error::EntryRefused);
            }
        }
        Ok((library, Entered { exit }))
    }

    /// As above, for Linux and the other systems that are neither Windows
    /// nor macOS: `ModuleEntry` is given the handle the library was opened
    /// with.
    ///
    /// # Safety
    ///
    /// As above.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub(super) unsafe fn enter(
        library: libloading::Library,
        _path: &Path,
    ) -> Result<(libloading::Library, Entered), Vst3Error> {
        type EntryFn = unsafe extern "system" fn(*mut std::ffi::c_void) -> bool;
        // SAFETY: per the contract, these symbols are VST 3's entry and exit
        // functions for Linux, which have these types.
        let (entry, exit) = unsafe {
            (
                library
                    .get::<EntryFn>(b"ModuleEntry\0")
                    .ok()
                    .map(|symbol| *symbol),
                library
                    .get::<ExitFn>(b"ModuleExit\0")
                    .ok()
                    .map(|symbol| *symbol),
            )
        };
        // The handle is taken out and put back; the library stays open.
        let handle = libloading::os::unix::Library::from(library).into_raw();
        // SAFETY: `handle` is the one just taken out of an open library.
        let library =
            libloading::Library::from(unsafe { libloading::os::unix::Library::from_raw(handle) });
        if let Some(entry) = entry {
            // SAFETY: the module's own entry function, called once, first,
            // with the handle it was opened with.
            if !unsafe { entry(handle) } {
                return Err(Vst3Error::EntryRefused);
            }
        }
        Ok((library, Entered { exit }))
    }

    /// As above, for macOS: `bundleEntry` is given the bundle, made with
    /// the system's own bundle functions.
    ///
    /// # Safety
    ///
    /// As above.
    #[cfg(target_os = "macos")]
    pub(super) unsafe fn enter(
        library: libloading::Library,
        path: &Path,
    ) -> Result<(libloading::Library, Entered), Vst3Error> {
        type EntryFn = unsafe extern "system" fn(*mut std::ffi::c_void) -> bool;
        // SAFETY: per the contract, these symbols are VST 3's entry and exit
        // functions for macOS, which have these types.
        let (entry, exit) = unsafe {
            (
                library
                    .get::<EntryFn>(b"bundleEntry\0")
                    .ok()
                    .map(|symbol| *symbol),
                library
                    .get::<ExitFn>(b"bundleExit\0")
                    .ok()
                    .map(|symbol| *symbol),
            )
        };
        let bundle = super::mac::Bundle::of(path);
        if let Some(entry) = entry {
            // SAFETY: the module's own entry function, called once, first,
            // with its bundle or with null for a bare library.
            if !unsafe { entry(bundle.as_ptr()) } {
                bundle.release();
                return Err(Vst3Error::EntryRefused);
            }
        }
        Ok((library, Entered { exit, bundle }))
    }

    /// Systems VST 3 does not define a module format for: nothing is
    /// called.
    ///
    /// # Safety
    ///
    /// None beyond loading the library, which has happened.
    #[cfg(not(any(windows, unix)))]
    pub(super) unsafe fn enter(
        library: libloading::Library,
        _path: &Path,
    ) -> Result<(libloading::Library, Entered), Vst3Error> {
        Ok((library, Entered { exit: None }))
    }
}

/// The bundle handle `bundleEntry` takes on macOS, made with CoreFoundation,
/// which every macOS program can link.
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicPtr, Ordering};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(object: *const c_void);
    }

    /// A `CFBundleRef`, or null, released once.
    pub(super) struct Bundle(AtomicPtr<c_void>);

    // A CoreFoundation object may be retained and released on any thread,
    // and this value does nothing else with it.
    // SAFETY: as above.
    unsafe impl Send for Bundle {}
    // SAFETY: as above; the pointer is behind an atomic.
    unsafe impl Sync for Bundle {}

    impl Bundle {
        pub(super) fn none() -> Self {
            Self(AtomicPtr::new(std::ptr::null_mut()))
        }

        /// The bundle at `path`, or none if `path` is not a folder or the
        /// system cannot make a bundle of it.
        pub(super) fn of(path: &Path) -> Self {
            if !path.is_dir() {
                return Self::none();
            }
            let Ok(absolute) = std::path::absolute(path) else {
                return Self::none();
            };
            let bytes = absolute.as_os_str().as_bytes();
            let Ok(length) = isize::try_from(bytes.len()) else {
                return Self::none();
            };
            // SAFETY: the default allocator (null), a buffer of `length`
            // bytes that outlives the call, and a flag; the result is an
            // object this function owns, or null.
            let url = unsafe {
                CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), length, 1)
            };
            if url.is_null() {
                return Self::none();
            }
            // SAFETY: the default allocator and a live URL; the result is
            // an object this value owns, or null. The URL is released once,
            // here, after the bundle has taken what it needs from it.
            let bundle = unsafe {
                let bundle = CFBundleCreate(std::ptr::null(), url);
                CFRelease(url);
                bundle
            };
            Self(AtomicPtr::new(bundle))
        }

        pub(super) fn as_ptr(&self) -> *mut c_void {
            self.0.load(Ordering::Acquire)
        }

        /// Releases the bundle. Does nothing the second time.
        pub(super) fn release(&self) {
            let bundle = self.0.swap(std::ptr::null_mut(), Ordering::AcqRel);
            if !bundle.is_null() {
                // SAFETY: an object this value owned, released once.
                unsafe { CFRelease(bundle) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_categories_are_words_between_bars() {
        assert_eq!(words("Instrument|Synth"), ["Instrument", "Synth"]);
        assert_eq!(words("Fx"), ["Fx"]);
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("|Fx||Delay |"), ["Fx", "Delay"]);
    }

    #[test]
    fn a_null_factory_is_an_error_and_nothing_more() {
        // SAFETY: null is allowed.
        let outcome = unsafe { Vst3Module::from_factory(std::ptr::null_mut(), "nothing") };
        assert_eq!(outcome.unwrap_err(), Vst3Error::NoFactory);
    }

    #[test]
    fn a_name_for_the_loader_is_absolute_and_keeps_its_extension() {
        let here = std::env::current_dir().unwrap();
        let named = exact_name(Path::new("plugin.vst3"));
        assert_eq!(PathBuf::from(named), here.join("plugin.vst3"));
        let bare = PathBuf::from(exact_name(Path::new("Contents/MacOS/Plugin")));
        if cfg!(windows) {
            assert_eq!(bare, here.join("Contents/MacOS/Plugin."));
        } else {
            assert_eq!(bare, here.join("Contents/MacOS/Plugin"));
        }
    }
}
