//! Loading a plugin's shared library and asking it what it contains.

use std::ffi::{CStr, CString, OsString, c_char};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::version::clap_version_is_compatible;

use super::text::bounded_text;
use super::{
    ClapError, ClapPlugin, HostInfo, MAX_FEATURES, MAX_PLUGINS_PER_LIBRARY, MAX_TEXT_BYTES, bundle,
};

/// The name of the symbol every CLAP library exports.
const ENTRY_SYMBOL: &[u8] = b"clap_entry\0";

/// What a plugin says about itself before it is created.
///
/// Every string is read with a limit of [`MAX_TEXT_BYTES`] and is valid
/// text whatever the plugin supplied. A field the plugin left out is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDescriptor {
    /// The plugin's identifier, in reverse-domain form. This is what
    /// [`ClapLibrary::create`] takes and what sessions store.
    pub id: String,
    /// The name shown to people.
    pub name: String,
    /// Who makes it.
    pub vendor: String,
    /// The vendor's or the product's web page.
    pub url: String,
    /// Where the manual is.
    pub manual_url: String,
    /// Where to ask for help.
    pub support_url: String,
    /// The plugin's version, as text.
    pub version: String,
    /// One line about what it does.
    pub description: String,
    /// What kind of plugin it is, as CLAP's feature words: `"instrument"`,
    /// `"audio-effect"`, `"stereo"` and so on. At most [`MAX_FEATURES`].
    pub features: Vec<String>,
    /// The version of CLAP the plugin was built for, as
    /// `(major, minor, revision)`.
    pub clap_version: (u32, u32, u32),
}

impl PluginDescriptor {
    /// Whether the plugin lists `feature` among its features.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|listed| listed == feature)
    }
}

/// A loaded CLAP library: one file that holds one or more plugins.
///
/// Loading runs the library's `init` once. Every plugin created from the
/// library keeps it loaded; when the last plugin and the last copy of this
/// handle are gone, `deinit` runs once and the library is unloaded. Loading
/// a file that is already loaded gives another handle to the same library
/// and does not run `init` again.
///
/// The handle can be cloned and sent between threads. CLAP requires a
/// library's entry point and factory to be safe to use from any thread, so
/// plugins can be listed on a worker thread while the main thread carries
/// on.
///
/// Loading is `unsafe`, and it is the only step that is: a library is code,
/// and what it does once it is called is not something this crate can
/// check. See [`load`](Self::load) for what the caller vouches for.
/// Everything done with the library afterwards is safe to call on the
/// strength of that.
///
/// ```
/// use manguebyte_host::clap::{ClapError, ClapLibrary};
///
/// // A path with nothing behind it is an error, not a crash.
/// // SAFETY: there is no file at this path, so no code is run.
/// let missing = unsafe { ClapLibrary::load("no/such/plugin.clap") };
/// assert!(matches!(missing, Err(ClapError::NotFound(_))));
/// ```
#[derive(Clone)]
pub struct ClapLibrary {
    inner: Arc<Inner>,
    /// What this handle's caller asked to load, a file or a bundle, as the
    /// caller wrote it. Two handles to one library, loaded through two
    /// spellings of its path, each keep their own.
    path: PathBuf,
}

struct Inner {
    entry: clap_plugin_entry,
    factory: *const clap_plugin_factory,
    factory_table: clap_plugin_factory,
    descriptors: Vec<PluginDescriptor>,
    /// Tells this library apart from every other one ever loaded.
    serial: u64,
    /// The operating system's handle, or `None` for an entry point that is
    /// part of the program itself.
    library: Option<libloading::Library>,
}

// SAFETY: `factory` points into the loaded library, which stays loaded for
// as long as this value exists, and CLAP requires every function of the
// entry point and of the factory to be callable from any thread.
unsafe impl Send for Inner {}
// SAFETY: as above.
unsafe impl Sync for Inner {}

/// A library that is loaded right now.
struct Loaded {
    /// The file it was loaded from, in a form that is the same however the
    /// path was written.
    key: PathBuf,
    serial: u64,
    library: Weak<Inner>,
}

/// Every library loaded from a file and not yet unloaded.
///
/// It exists so that `init` and `deinit` stay balanced when the same file
/// is asked for twice: the second request shares the first one's library.
/// The lock is also held across `init`, `deinit` and unloading, so these
/// never overlap for two requests for the same file.
static REGISTRY: Mutex<Vec<Loaded>> = Mutex::new(Vec::new());
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

fn registry() -> MutexGuard<'static, Vec<Loaded>> {
    REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner())
}

impl ClapLibrary {
    /// Loads a CLAP library from a file, or from a macOS-style bundle
    /// folder, and reads the list of plugins in it.
    ///
    /// For a bundle (a folder), the executable is the one its
    /// `Contents/Info.plist` names, or failing that the file in
    /// `Contents/MacOS` named after the bundle. Only XML property lists
    /// are understood.
    ///
    /// The library's `init` is told where the library is as an absolute
    /// path, whatever form `path` has, so that a plugin which finds its
    /// resources by that path does not depend on the working directory.
    /// [`path`](Self::path) returns `path` as it was given.
    ///
    /// # Errors
    ///
    /// Each way this can go wrong has its own [`ClapError`]: nothing at the
    /// path, a file the system cannot load as a library, a library with no
    /// CLAP entry point, an entry point for an incompatible version of
    /// CLAP, an `init` that refuses, and a library with no plugin factory.
    /// In every case nothing stays loaded.
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
    /// - If the library exports a symbol named `clap_entry`, that symbol
    ///   must be a `clap_plugin_entry`, and the functions reached through
    ///   it (the entry point, the factory, every plugin and every
    ///   extension) must follow the CLAP specification: this host checks
    ///   what they return, but it has to call them.
    ///
    /// These are the terms of [`from_entry`](Self::from_entry), for an
    /// entry point that comes out of a file. Every safe function of the
    /// library and of the plugins created from it relies on them.
    ///
    /// A path with nothing at it, and a file the system refuses to load as
    /// a library, meet these terms as they are: no code is run, and the
    /// result is an error. A library that has no `clap_entry` is an error
    /// too, but it is loaded to find that out, so the first term has to
    /// hold for it.
    ///
    /// Only load files you would be willing to run.
    pub unsafe fn load(path: impl AsRef<Path>) -> Result<Self, ClapError> {
        let path = path.as_ref();
        let binary = bundle::binary_of(path)?;
        let key = std::fs::canonicalize(&binary).unwrap_or_else(|_| binary.clone());
        loop {
            let mut registry = registry();
            let Some(loaded) = registry.iter().find(|loaded| loaded.key == key) else {
                let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
                // SAFETY: passed on from the caller, who vouches for the
                // file at `path` and so for its executable `binary`.
                let inner = Arc::new(unsafe { open(path, &binary, serial) }?);
                registry.push(Loaded {
                    key,
                    serial,
                    library: Arc::downgrade(&inner),
                });
                return Ok(Self {
                    inner,
                    path: path.to_owned(),
                });
            };
            if let Some(inner) = loaded.library.upgrade() {
                return Ok(Self {
                    inner,
                    path: path.to_owned(),
                });
            }
            // The last user of that library is letting go of it at this
            // moment, on another thread, and is waiting for the lock to
            // unload it. Step aside until it has.
            drop(registry);
            std::thread::yield_now();
        }
    }

    /// Wraps an entry point that is part of this program instead of a file
    /// on disk: a plugin linked in statically, or one defined in a test.
    ///
    /// `path` is what the entry point's `init` is told its location is,
    /// passed on exactly as given, and what [`path`](Self::path) returns.
    ///
    /// ```
    /// use std::ffi::{CStr, c_char, c_void};
    /// use manguebyte_host::clap::ClapLibrary;
    /// use manguebyte_host::clap::clap_sys::entry::clap_plugin_entry;
    /// use manguebyte_host::clap::clap_sys::factory::plugin_factory::{
    ///     CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory,
    /// };
    /// use manguebyte_host::clap::clap_sys::version::CLAP_VERSION;
    ///
    /// // The smallest library there is: it initialises and offers nothing.
    /// unsafe extern "C" fn init(_path: *const c_char) -> bool {
    ///     true
    /// }
    /// unsafe extern "C" fn deinit() {}
    /// unsafe extern "C" fn count(_factory: *const clap_plugin_factory) -> u32 {
    ///     0
    /// }
    /// static FACTORY: clap_plugin_factory = clap_plugin_factory {
    ///     get_plugin_count: Some(count),
    ///     get_plugin_descriptor: None,
    ///     create_plugin: None,
    /// };
    /// unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    ///     // SAFETY: a host passes a NUL-terminated factory id.
    ///     if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
    ///         (&raw const FACTORY).cast()
    ///     } else {
    ///         std::ptr::null()
    ///     }
    /// }
    /// static ENTRY: clap_plugin_entry = clap_plugin_entry {
    ///     clap_version: CLAP_VERSION,
    ///     init: Some(init),
    ///     deinit: Some(deinit),
    ///     get_factory: Some(get_factory),
    /// };
    ///
    /// // SAFETY: the entry point is a static and does what CLAP asks.
    /// let library = unsafe { ClapLibrary::from_entry(&raw const ENTRY, "built in") }?;
    /// assert!(library.descriptors().is_empty());
    /// assert!(library.create("org.example.nothing").is_err());
    /// # Ok::<(), manguebyte_host::clap::ClapError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// As for [`load`](Self::load), without the ones about files.
    ///
    /// # Safety
    ///
    /// `entry` must point to a `clap_plugin_entry` that stays valid for the
    /// rest of the program, and whose functions follow the CLAP
    /// specification: this host checks what they return, but it has to
    /// call them.
    pub unsafe fn from_entry(
        entry: *const clap_plugin_entry,
        path: &str,
    ) -> Result<Self, ClapError> {
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(path);
        // SAFETY: passed on from the caller.
        let inner = unsafe { initialise(entry, path, None, serial) }?;
        Ok(Self {
            inner: Arc::new(inner),
            path: path.to_owned(),
        })
    }

    /// The path this library was loaded from, as it was given to
    /// [`load`](Self::load) or [`from_entry`](Self::from_entry) for this
    /// handle and its clones. A file loaded twice, through two spellings
    /// of its path, is one library with two handles, and each handle
    /// reports the spelling it was loaded with.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The version of CLAP the library was built for, as
    /// `(major, minor, revision)`.
    pub fn clap_version(&self) -> (u32, u32, u32) {
        let version = self.inner.entry.clap_version;
        (version.major, version.minor, version.revision)
    }

    /// The plugins in this library, as read when it was loaded.
    ///
    /// At most [`MAX_PLUGINS_PER_LIBRARY`] are listed. A plugin with no
    /// identifier, or built for an incompatible version of CLAP, is left
    /// out, since it could not be created.
    pub fn descriptors(&self) -> &[PluginDescriptor] {
        &self.inner.descriptors
    }

    /// The descriptor of the plugin with this identifier, if the library
    /// has one.
    pub fn descriptor(&self, id: &str) -> Option<&PluginDescriptor> {
        self.inner
            .descriptors
            .iter()
            .find(|descriptor| descriptor.id == id)
    }

    /// Creates the plugin with this identifier.
    ///
    /// The calling thread becomes the plugin's main thread: the returned
    /// handle cannot leave it. The plugin is told it is hosted by
    /// [`HostInfo::default`].
    ///
    /// # Errors
    ///
    /// [`ClapError::UnknownPlugin`] if the library lists no plugin with
    /// this identifier; otherwise an error describing what the plugin
    /// refused or got wrong. A plugin that fails to initialise is
    /// destroyed again.
    pub fn create(&self, id: &str) -> Result<ClapPlugin, ClapError> {
        self.create_with(id, &HostInfo::default())
    }

    /// Creates a plugin as [`create`](Self::create) does, telling it the
    /// name and version of the program hosting it.
    ///
    /// # Errors
    ///
    /// As for [`create`](Self::create).
    pub fn create_with(&self, id: &str, host: &HostInfo) -> Result<ClapPlugin, ClapError> {
        ClapPlugin::create(self.clone(), id, host)
    }

    /// Asks the factory for a plugin. The result may be null.
    ///
    /// # Safety
    ///
    /// `host` must stay valid until the returned plugin has been destroyed.
    pub(crate) unsafe fn create_raw(
        &self,
        host: *const clap_host,
        id: &CStr,
    ) -> *const clap_plugin {
        let Some(create) = self.inner.factory_table.create_plugin else {
            return std::ptr::null();
        };
        // SAFETY: the factory belongs to this library, which is loaded and
        // initialised; `host` is valid per the contract and `id` is a C
        // string.
        unsafe { create(self.inner.factory, host, id.as_ptr()) }
    }
}

impl std::fmt::Debug for ClapLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapLibrary")
            .field("path", &self.path)
            .field("plugins", &self.inner.descriptors.len())
            .finish()
    }
}

impl PartialEq for ClapLibrary {
    /// Two handles are equal when they stand for the same loaded library.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for ClapLibrary {}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut registry = registry();
        if let Some(deinit) = self.entry.deinit {
            // SAFETY: `init` succeeded when this value was made, and this
            // is the one `deinit` that balances it. Every plugin created
            // from the library held a reference to this value, so none is
            // left.
            unsafe { deinit() };
        }
        // Unloaded while the lock is still held, so that a request for the
        // same file finds the library either whole or gone.
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
/// executable inside a bundle has not; a dot at the end tells it not to.
fn exact_name(binary: &Path) -> OsString {
    let absolute = std::path::absolute(binary).unwrap_or_else(|_| binary.to_owned());
    let needs_dot = cfg!(windows) && absolute.extension().is_none();
    let mut name = absolute.into_os_string();
    if needs_dot {
        name.push(".");
    }
    name
}

/// Where a library loaded from `path` is told it is: `path` made absolute,
/// without touching the file system and without resolving links, so a
/// bundle stays the bundle the caller named.
///
/// CLAP has `init` receive the path of the library, or of its bundle, and
/// a plugin may keep it to find the files that come with it. Given the
/// path as the caller wrote it, a relative one would send the plugin
/// looking from wherever the working directory is at that later moment.
fn location(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// Loads `binary`, the executable of the library or bundle at `path`, and
/// initialises it. Called with the registry locked.
///
/// # Safety
///
/// As for [`ClapLibrary::load`], whose two terms are the two obligations
/// taken on here: running the library's own code as the system loads it,
/// and reading its `clap_entry` as a `clap_plugin_entry`.
unsafe fn open(path: &Path, binary: &Path, serial: u64) -> Result<Inner, ClapError> {
    // SAFETY: loading a library runs its initialisation routines. Per this
    // function's contract, which is the first term of `ClapLibrary::load`,
    // the caller vouches that for this file they are sound to run here.
    let library = unsafe { libloading::Library::new(exact_name(binary)) }.map_err(|error| {
        ClapError::Load {
            path: binary.to_owned(),
            reason: error.to_string(),
        }
    })?;
    // SAFETY: the symbol is asked for as an address, a type that fits any
    // symbol, and nothing is read through it here.
    let entry = match unsafe { library.get::<*const clap_plugin_entry>(ENTRY_SYMBOL) } {
        Ok(symbol) => *symbol,
        Err(_) => return Err(ClapError::NoEntryPoint(binary.to_owned())),
    };
    // SAFETY: per this function's contract, which is the second term of
    // `ClapLibrary::load`, a symbol of this name is a `clap_plugin_entry`
    // whose functions follow the CLAP specification. It points into
    // `library`, which is moved into the value this returns and so stays
    // loaded for as long as that value is used.
    unsafe { initialise(entry, &location(path), Some(library), serial) }
}

/// Checks an entry point, runs its `init` and reads its plugin list.
///
/// `location` is what its `init` is told its path is. What the library is
/// known by afterwards is kept by each handle, not here.
///
/// # Safety
///
/// `entry` must be null or point to a `clap_plugin_entry` that stays valid
/// for as long as `library` is loaded, or for the rest of the program if
/// `library` is `None`, and whose functions follow the CLAP specification.
unsafe fn initialise(
    entry: *const clap_plugin_entry,
    location: &Path,
    library: Option<libloading::Library>,
    serial: u64,
) -> Result<Inner, ClapError> {
    if entry.is_null() || !entry.is_aligned() {
        return Err(ClapError::MalformedEntry);
    }
    // SAFETY: not null, aligned, and valid per the contract.
    let entry = unsafe { *entry };
    let version = entry.clap_version;
    if !clap_version_is_compatible(version) {
        return Err(ClapError::IncompatibleVersion {
            major: version.major,
            minor: version.minor,
            revision: version.revision,
        });
    }
    let (Some(init), Some(deinit), Some(get_factory)) =
        (entry.init, entry.deinit, entry.get_factory)
    else {
        return Err(ClapError::MalformedEntry);
    };

    // CLAP gives the plugin its own path as UTF-8. A path that is not
    // valid Unicode is passed as closely as text can express it.
    let c_path = CString::new(location.to_string_lossy().replace('\0', "")).unwrap_or_default();
    // SAFETY: the entry point's own function, called before any other, with
    // a NUL-terminated path.
    if !unsafe { init(c_path.as_ptr()) } {
        // After a refused `init`, `deinit` must not be called.
        return Err(ClapError::InitFailed);
    }

    // SAFETY: `init` succeeded, and the id is a C string.
    let factory =
        unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) }.cast::<clap_plugin_factory>();
    if factory.is_null() || !factory.is_aligned() {
        // SAFETY: balances the `init` above; nothing else was created.
        unsafe { deinit() };
        return Err(ClapError::NoFactory);
    }
    // SAFETY: not null, aligned, and returned by the library as its plugin
    // factory, which stays valid until `deinit`.
    let factory_table = unsafe { *factory };
    // SAFETY: `factory` is the factory `factory_table` was read from.
    let descriptors = unsafe { read_descriptors(factory, &factory_table) };

    Ok(Inner {
        entry,
        factory,
        factory_table,
        descriptors,
        serial,
        library,
    })
}

/// Lists the plugins a factory offers.
///
/// # Safety
///
/// `factory` must be the live factory that `table` was read from.
unsafe fn read_descriptors(
    factory: *const clap_plugin_factory,
    table: &clap_plugin_factory,
) -> Vec<PluginDescriptor> {
    let (Some(count), Some(get)) = (table.get_plugin_count, table.get_plugin_descriptor) else {
        return Vec::new();
    };
    // SAFETY: the factory's own function with the factory's own pointer.
    let count = (unsafe { count(factory) } as usize).min(MAX_PLUGINS_PER_LIBRARY);
    let mut found: Vec<PluginDescriptor> = Vec::new();
    for index in 0..count {
        // SAFETY: as above; `index` is below the count just reported.
        let descriptor = unsafe { get(factory, index as u32) };
        if descriptor.is_null() || !descriptor.is_aligned() {
            continue;
        }
        // SAFETY: not null, aligned, and CLAP keeps descriptors valid for
        // as long as the library is initialised.
        let descriptor = unsafe { read_descriptor(&*descriptor) };
        let usable = !descriptor.id.is_empty()
            && descriptor.clap_version.0 >= 1
            && !found.iter().any(|other| other.id == descriptor.id);
        if usable {
            found.push(descriptor);
        }
    }
    found
}

/// Copies a descriptor into owned, bounded text.
///
/// # Safety
///
/// Every string pointer in `raw` must be null or NUL-terminated, and
/// `features` must be null or an array of such pointers ending in null.
unsafe fn read_descriptor(raw: &clap_plugin_descriptor) -> PluginDescriptor {
    // SAFETY: each pointer is null or a C string, per the contract.
    let text = |pointer: *const c_char| unsafe { bounded_text(pointer, MAX_TEXT_BYTES) };
    let mut features = Vec::new();
    if !raw.features.is_null() && raw.features.is_aligned() {
        for index in 0..MAX_FEATURES {
            // SAFETY: no entry before this one was null, so the array has
            // not ended and this entry is part of it.
            let feature = unsafe { raw.features.add(index).read() };
            if feature.is_null() {
                break;
            }
            features.push(text(feature));
        }
    }
    PluginDescriptor {
        id: text(raw.id),
        name: text(raw.name),
        vendor: text(raw.vendor),
        url: text(raw.url),
        manual_url: text(raw.manual_url),
        support_url: text(raw.support_url),
        version: text(raw.version),
        description: text(raw.description),
        features,
        clap_version: (
            raw.clap_version.major,
            raw.clap_version.minor,
            raw.clap_version.revision,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use clap_sys::version::CLAP_VERSION;

    use super::*;

    #[test]
    fn a_library_is_located_by_an_absolute_path_however_it_was_named() {
        let here = std::env::current_dir().unwrap();

        // A relative path is counted from the working directory, now.
        let relative = Path::new("plugins").join("Some Plugin.clap");
        let located = location(&relative);
        assert!(located.is_absolute());
        assert_eq!(located, here.join(&relative));
        // So is a bare file name, which a loader left to itself would look
        // up in the system's folders.
        assert_eq!(location(Path::new("plain.clap")), here.join("plain.clap"));

        // An absolute path is passed on as it is.
        let absolute = here.join("somewhere").join("Bundle.clap");
        assert_eq!(location(&absolute), absolute);

        // A path that cannot be made absolute is passed on as it is.
        assert_eq!(location(Path::new("")), Path::new(""));
    }

    /// What the entry point below was last told its path is.
    static TOLD: Mutex<String> = Mutex::new(String::new());

    unsafe extern "C" fn init(path: *const c_char) -> bool {
        // SAFETY: the host passes a NUL-terminated path.
        let path = unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned();
        *TOLD.lock().unwrap() = path;
        true
    }

    unsafe extern "C" fn deinit() {}

    unsafe extern "C" fn no_factory(_id: *const c_char) -> *const c_void {
        std::ptr::null()
    }

    static ENTRY: clap_plugin_entry = clap_plugin_entry {
        clap_version: CLAP_VERSION,
        init: Some(init),
        deinit: Some(deinit),
        get_factory: Some(no_factory),
    };

    #[test]
    fn init_is_told_the_location_and_not_the_name_the_caller_used() {
        // What the caller wrote, `plugins/as written.clap`, is made into
        // this by `location`, tested above; `initialise` never sees the
        // name as written.
        let located = Path::new("/the/whole/way/to/plugins/as written.clap");
        // SAFETY: the entry point is the static above, whose functions do
        // what CLAP asks of them.
        let outcome = unsafe { initialise(&raw const ENTRY, located, None, 0) };
        // It has no factory, so it is initialised, found empty and put
        // away, which is enough to see what `init` received.
        assert!(matches!(outcome, Err(ClapError::NoFactory)));
        assert_eq!(
            *TOLD.lock().unwrap(),
            "/the/whole/way/to/plugins/as written.clap"
        );
    }
}
