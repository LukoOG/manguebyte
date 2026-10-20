//! A document shared between an editor and whoever saves state, and saved
//! with a plugin's parameters.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use manguebyte_params::ExtraState;

use crate::{Document, Migrations, VersionWatch};

/// A [`Document`] behind a mutex, shared between threads, that a plugin's
/// parameters can carry so that its saved state includes the document.
///
/// Put it in the parameter struct, list it after `extra` in `param_set!`,
/// and `manguebyte_plugin::save_state` writes it after the parameter
/// values, as a section named `document` (or [`with_section`](Self::with_section)'s
/// name) holding [`Document::to_text`]. Restoring state reads it with
/// [`Document::from_text_with`] and puts it in place with
/// [`Document::replace`]: the history is cleared, since a host's restore is
/// the host's to undo, and the parameter link is kept.
///
/// The mutex is only ever taken by the editor and by the thread a host
/// saves and restores state on, never by the audio thread, which gets
/// snapshots ([`snapshot`](crate::snapshot)) instead.
///
/// ```
/// use std::sync::Arc;
/// use manguebyte_params::{FloatParam, FloatRange, ParamSet, param_set};
/// use manguebyte_state::{Document, SharedDocument};
///
/// struct Params { cutoff: FloatParam, doc: SharedDocument }
/// param_set!(Params { cutoff, extra doc });
///
/// let params = Arc::new(Params {
///     cutoff: FloatParam::new("cutoff", "Cutoff", FloatRange::logarithmic(20.0, 20_000.0), 1_000.0),
///     doc: SharedDocument::new(Document::new("instrument")),
/// });
/// {
///     let mut doc = params.doc.lock();
///     let mirror = doc.edit("Set up", |tx| tx.add_child(tx.root(), "params"))?;
///     doc.link_params(&params, mirror)?;
///     doc.clear_history();
/// }
///
/// // A user edit to the parameter is undoable; it reaches the atomic.
/// params.doc.lock().edit("Change cutoff", |tx| tx.set_param("cutoff", 440.0))?;
/// assert_eq!(params.cutoff.value(), 440.0);
/// params.doc.lock().undo();
/// assert_eq!(params.cutoff.value(), 1_000.0);
///
/// // The document travels with the parameter values.
/// let mut sections = Vec::new();
/// params.save_extra(&mut |name, _text| sections.push(name.to_owned()));
/// assert_eq!(sections, ["document"]);
/// # Ok::<(), manguebyte_state::EditError>(())
/// ```
#[derive(Clone)]
pub struct SharedDocument {
    doc: Arc<Mutex<Document>>,
    watch: VersionWatch,
    section: Arc<str>,
    migrations: Option<Arc<Migrations>>,
}

impl fmt::Debug for SharedDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedDocument")
            .field("section", &self.section)
            .field("version", &self.watch.version())
            .finish_non_exhaustive()
    }
}

impl SharedDocument {
    /// Shares `doc`, saved under the name `document`.
    pub fn new(doc: Document) -> Self {
        Self {
            watch: doc.watch(),
            doc: Arc::new(Mutex::new(doc)),
            section: Arc::from("document"),
            migrations: None,
        }
    }

    /// Saves the document under `name` instead, for a plugin that keeps
    /// more than one.
    ///
    /// # Panics
    ///
    /// Panics if `name` is not a valid name (see
    /// [`is_valid_name`](crate::is_valid_name)).
    #[must_use]
    pub fn with_section(mut self, name: &str) -> Self {
        assert!(crate::is_valid_name(name), "invalid section name {name:?}");
        self.section = Arc::from(name);
        self
    }

    /// Brings documents of an older schema up to date when state is
    /// restored.
    #[must_use]
    pub fn with_migrations(mut self, migrations: Migrations) -> Self {
        self.migrations = Some(Arc::new(migrations));
        self
    }

    /// Locks the document. Never call this on the audio thread.
    ///
    /// A panic while the lock was held does not make the document
    /// unusable: every edit leaves it whole, so the lock is taken anyway.
    pub fn lock(&self) -> MutexGuard<'_, Document> {
        self.doc.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The document's version, without locking it: lock only when it has
    /// moved.
    pub fn version(&self) -> u64 {
        self.watch.version()
    }

    /// A watch on the document's version, for another thread.
    pub fn watch(&self) -> VersionWatch {
        self.watch.clone()
    }

    /// The name the document is saved under.
    pub fn section(&self) -> &str {
        &self.section
    }
}

impl ExtraState for SharedDocument {
    fn save_extra(&self, section: &mut dyn FnMut(&str, &str)) {
        let text = self.lock().to_text();
        section(&self.section, &text);
    }

    fn load_extra(&self, name: &str, text: &str) -> bool {
        if name != &*self.section {
            return false;
        }
        let loaded = match &self.migrations {
            Some(migrations) => Document::from_text_with(text, migrations),
            None => Document::from_text(text),
        };
        match loaded {
            Ok((doc, _)) => {
                self.lock().replace(doc);
                true
            }
            // Text that is not a document leaves the document as it was.
            Err(_) => false,
        }
    }
}
