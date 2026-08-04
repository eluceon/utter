//! Maps each [`LanguageProfile`]'s hotkey binding to its own engines,
//! building them lazily on first use rather than at boot.
//!
//! ## Why lazy
//!
//! The models a profile can select together weigh about a gigabyte, the app
//! sits in the tray all day, and most sessions only ever speak one language.
//! Loading every configured profile's engine at boot would make a bilingual
//! setup cost more idle memory than a monolingual one, even for someone who
//! never presses the second hotkey. [`ProfileRegistry`] instead builds a
//! profile's [`ProfileDeps`] the first time [`ProfileRegistry::deps_for`] is
//! asked for its binding, and keeps the result for every call after that.
//!
//! ## Why failure isolation matters here
//!
//! Before profiles there was one engine: if it failed to load, dictation
//! didn't work, and that was the whole story. With more than one profile, a
//! broken model for one language must not take a healthy one down with it.
//! A profile whose model is missing or damaged still resolves to `Some` from
//! [`ProfileRegistry::deps_for`], carrying the same `unavailable_engine`
//! stand-in [`crate::runtime_boot::build_engine`] already falls back to,
//! plus a notice explaining why — never a load that poisons the whole
//! registry or one that silently disables another profile's hotkey. `None`
//! from `deps_for` means only one thing: no binding with that id exists.
//!
//! ## The `_Exit()` trap
//!
//! `runtime_boot::build_sherpa` calls `ModelManager::verify_installed`
//! before ever handing a path to sherpa-onnx: a corrupt model file makes
//! sherpa's C++ layer call `_Exit()`, which takes the whole process down —
//! every profile, healthy ones included, with no chance for Rust to catch
//! it. [`RealProfileLoader`] reuses `runtime_boot::build_engine` (which
//! reuses `build_sherpa`) rather than reimplementing engine construction, so
//! that check is never bypassed by a profile-specific load path.

use std::sync::Arc;

use utter_core::{SttEngine, TextRefiner, Tone};
use utter_inject::BindingId;
use utter_store::settings::{refinement_is_on, RefineCfg};
use utter_store::{LanguageProfile, ModelManager};

use crate::runtime_boot::{build_engine, build_refiner, engine_label, QueuedNotice};

/// The per-profile slice of what a dictation session needs: everything
/// [`crate::runtime::RuntimeDeps`] does *not* already own once and share
/// across every profile (the hotkey receiver, history connection, capture
/// backend, injector, ...). See the "Amended 2026-08-04" note on Task 15 in
/// the v0.2 plan for why this is a separate type rather than one
/// `RuntimeDeps` per profile.
pub struct ProfileDeps {
    pub engine: Box<dyn SttEngine>,
    pub refiner: Option<Box<dyn TextRefiner>>,
    pub refine_enabled: bool,
    pub tone: Tone,
    pub language: Option<String>,
    /// Recorded on each history entry (e.g. `"whisper"`, `"sherpa"`, `"cloud"`).
    pub engine_label: String,
    pub dictionary_terms: Vec<String>,
}

/// Turns one [`LanguageProfile`] into its [`ProfileDeps`], plus any
/// degradation notices to surface (mirrors
/// [`crate::runtime_boot::build_deps`]'s notice convention). Injected into
/// [`ProfileRegistry`] so it is unit-testable with no models on disk — the
/// production implementation is [`RealProfileLoader`].
///
/// `Send` because [`ProfileRegistry`] is destined to live on the dictation
/// worker thread (Task 16), so whatever it holds must be movable there.
pub trait ProfileLoader: Send {
    fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<QueuedNotice>);
}

/// Production [`ProfileLoader`]: builds real engines and refiners via
/// `runtime_boot`'s existing builders, so every degrade-don't-fail path a
/// single-engine boot already has (missing model, damaged sherpa model, an
/// unsupported build, ...) — including the `verify_installed` check that
/// keeps a corrupt sherpa model from calling `_Exit()` — applies per profile
/// too, rather than being reimplemented here.
pub struct RealProfileLoader {
    models: Arc<ModelManager>,
    /// The global refine settings ([`RefineCfg`]): the master switch, plus
    /// the endpoint/model/timeout a profile has no per-language override
    /// for yet. Combined with each profile's own [`RefinePolicy`] via
    /// [`refinement_is_on`] — the one place that combination happens.
    ///
    /// [`RefinePolicy`]: utter_store::profile::RefinePolicy
    global_refine: RefineCfg,
    /// The user's dictionary terms, fed to whichever engine a profile
    /// builds as a recognition hint. Global rather than per-profile: there
    /// is no per-profile dictionary yet.
    dictionary_terms: Vec<String>,
}

impl RealProfileLoader {
    pub fn new(
        models: Arc<ModelManager>,
        global_refine: RefineCfg,
        dictionary_terms: Vec<String>,
    ) -> Self {
        Self {
            models,
            global_refine,
            dictionary_terms,
        }
    }
}

impl ProfileLoader for RealProfileLoader {
    fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<QueuedNotice>) {
        let mut notices = Vec::new();

        let (engine, engine_notice) =
            build_engine(&profile.engine, &self.models, &self.dictionary_terms);
        if let Some(msg) = engine_notice {
            notices.push(("warning", msg));
        }

        let (refiner, refiner_notice) =
            build_refiner(&self.global_refine, self.dictionary_terms.clone());
        if let Some(msg) = refiner_notice {
            notices.push(("info", msg));
        }

        let deps = ProfileDeps {
            engine,
            refiner,
            refine_enabled: refinement_is_on(&self.global_refine, &profile.refine),
            tone: profile.refine.tone,
            language: Some(profile.language.clone()),
            engine_label: engine_label(profile.engine.active).to_string(),
            dictionary_terms: self.dictionary_terms.clone(),
        };

        (deps, notices)
    }
}

/// One configured profile, plus its engines once loaded.
struct Entry {
    profile: LanguageProfile,
    deps: Option<ProfileDeps>,
}

/// Maps each configured [`LanguageProfile`]'s hotkey binding to its engines,
/// building them lazily on first use — see the module doc comment.
///
/// [`BindingId`]s are assigned by position in the `profiles` list `new` was
/// given, the same order Task 16 registers their chords in via
/// `utter_inject::create_source`, so a binding's index here always lines up
/// with the id `create_source` hands back for it.
pub struct ProfileRegistry {
    loader: Box<dyn ProfileLoader>,
    entries: Vec<Entry>,
}

impl ProfileRegistry {
    /// Builds a registry over `profiles`, eagerly loading only the first one
    /// (conventionally the default/primary profile). Laziness is the whole
    /// point of this type, but a session with *nothing* loaded could not
    /// dictate a single word until the first hotkey press finished loading
    /// its engine, so the default is warmed up immediately, exactly as the
    /// single-engine boot path does today.
    pub fn new(
        profiles: Vec<LanguageProfile>,
        loader: Box<dyn ProfileLoader>,
    ) -> (Self, Vec<QueuedNotice>) {
        let entries: Vec<Entry> = profiles
            .into_iter()
            .map(|profile| Entry {
                profile,
                deps: None,
            })
            .collect();

        let mut registry = Self { loader, entries };

        let notices = if registry.entries.is_empty() {
            Vec::new()
        } else {
            registry.ensure_loaded(0)
        };

        (registry, notices)
    }

    /// Resolves `id` to its profile's engines, building them on first use.
    ///
    /// `None` means no binding with that id exists. A profile whose model
    /// is missing or damaged still resolves to `Some`, carrying the usual
    /// `unavailable_engine` stand-in plus a notice — see the module doc
    /// comment. The returned notices are only ever non-empty on the call
    /// that actually triggers the load; a profile already loaded (healthy
    /// or not) returns an empty list on every subsequent call, since its
    /// notice was already surfaced once.
    pub fn deps_for(&mut self, id: BindingId) -> Option<(&mut ProfileDeps, Vec<QueuedNotice>)> {
        let index = id.index();
        self.entries.get(index)?;

        let notices = self.ensure_loaded(index);
        let deps = self.entries[index]
            .deps
            .as_mut()
            .expect("invariant: ensure_loaded always leaves this entry's deps as Some");

        Some((deps, notices))
    }

    /// Loads `entries[index]`'s engines if it hasn't been loaded yet;
    /// returns the notices from that load, or an empty list if it was
    /// already loaded.
    fn ensure_loaded(&mut self, index: usize) -> Vec<QueuedNotice> {
        if self.entries[index].deps.is_some() {
            return Vec::new();
        }
        let (deps, notices) = self.loader.load(&self.entries[index].profile);
        self.entries[index].deps = Some(deps);
        notices
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use utter_store::profile::LanguageProfile;

    use super::*;
    use crate::runtime_boot::unavailable_engine;

    fn profile(id: &str) -> LanguageProfile {
        LanguageProfile {
            id: id.to_string(),
            ..LanguageProfile::default()
        }
    }

    fn fake_deps(engine_label: &str) -> ProfileDeps {
        ProfileDeps {
            engine: unavailable_engine("fake engine, never asked to transcribe".to_string()),
            refiner: None,
            refine_enabled: false,
            tone: Tone::Clean,
            language: None,
            engine_label: engine_label.to_string(),
            dictionary_terms: Vec::new(),
        }
    }

    /// A loader that counts how many times it was asked to build a profile,
    /// so laziness can be pinned as a number rather than inferred from
    /// timing. Always succeeds.
    struct CountingLoader {
        count: Arc<AtomicUsize>,
    }

    impl ProfileLoader for CountingLoader {
        fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<QueuedNotice>) {
            self.count.fetch_add(1, Ordering::SeqCst);
            (fake_deps(&profile.id), Vec::new())
        }
    }

    /// Wraps a [`ProfileRegistry`] together with the counter its
    /// [`CountingLoader`] bumps, so tests can assert on how many times
    /// profiles were actually built without `ProfileRegistry` itself
    /// needing a test-only accessor.
    struct CountingRegistry {
        registry: ProfileRegistry,
        count: Arc<AtomicUsize>,
    }

    impl CountingRegistry {
        fn deps_for(&mut self, id: BindingId) -> Option<(&mut ProfileDeps, Vec<QueuedNotice>)> {
            self.registry.deps_for(id)
        }

        fn load_count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    fn test_registry_with_counting_loader() -> CountingRegistry {
        let count = Arc::new(AtomicUsize::new(0));
        let loader = Box::new(CountingLoader {
            count: count.clone(),
        });
        let profiles = vec![profile("russian"), profile("english")];
        let (registry, _notices) = ProfileRegistry::new(profiles, loader);
        CountingRegistry { registry, count }
    }

    /// A loader where the profile named `"english"` always fails to
    /// produce a real engine (mirroring a missing/damaged model), while
    /// every other profile loads cleanly.
    struct FailingLoader;

    impl ProfileLoader for FailingLoader {
        fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<QueuedNotice>) {
            if profile.id == "english" {
                let reason = "english profile's model is not downloaded".to_string();
                let mut deps = fake_deps(&profile.id);
                deps.engine = unavailable_engine(reason.clone());
                (deps, vec![("warning", reason)])
            } else {
                (fake_deps(&profile.id), Vec::new())
            }
        }
    }

    fn test_registry_where_profile_one_fails() -> ProfileRegistry {
        // Index 0 ("russian") is healthy and is the one `new` eagerly
        // loads; index 1 ("english") is the one that fails, and only loads
        // (and fails) once its binding is actually asked for.
        let profiles = vec![profile("russian"), profile("english")];
        let (registry, _notices) = ProfileRegistry::new(profiles, Box::new(FailingLoader));
        registry
    }

    #[test]
    fn a_profile_loads_its_engines_only_on_first_use() {
        let mut registry = test_registry_with_counting_loader();
        assert_eq!(
            registry.load_count(),
            1,
            "only the default profile loads at boot"
        );

        registry.deps_for(BindingId::from(1));
        assert_eq!(registry.load_count(), 2);

        registry.deps_for(BindingId::from(1));
        assert_eq!(registry.load_count(), 2, "a loaded profile is not rebuilt");
    }

    #[test]
    fn a_broken_profile_does_not_disable_the_others() {
        let mut registry = test_registry_where_profile_one_fails();

        // The broken profile still resolves, so its hotkey reports why instead of
        // doing nothing -- but it reports, and it does not poison the registry.
        let (_, notices) = registry
            .deps_for(BindingId::from(1))
            .expect("binding exists");
        assert!(!notices.is_empty(), "a failed load must say so");

        let (_, notices) = registry
            .deps_for(BindingId::from(0))
            .expect("a broken English profile must not take Russian down with it");
        assert!(notices.is_empty(), "the healthy profile loads cleanly");
    }

    #[test]
    fn an_unknown_binding_resolves_to_nothing() {
        let mut registry = test_registry_with_counting_loader();
        assert!(registry.deps_for(BindingId::from(99)).is_none());
    }
}
