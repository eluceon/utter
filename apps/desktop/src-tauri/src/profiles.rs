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
    /// `Arc` rather than `Box`: `ProfileRegistry` caches a profile's `ProfileDeps` forever once
    /// loaded (see its own doc comment), so the worker needs to hand out a cheap clone of the
    /// refiner on every press of the same binding — a `refine_with_timeout` call races it on a
    /// detached thread (see `crate::runtime`) — rather than being able to move a `Box` out of a
    /// value it doesn't own.
    pub refiner: Option<Arc<dyn TextRefiner>>,
    pub refine_enabled: bool,
    pub tone: Tone,
    pub language: Option<String>,
    /// Recorded on each history entry (e.g. `"whisper"`, `"sherpa"`, `"cloud"`).
    pub engine_label: String,
    /// [`LanguageProfile::id`] of the profile these deps were built from. Recorded on each
    /// history entry alongside `engine_label` so two profiles on the same engine kind — the
    /// normal bilingual case, both on sherpa — can still be told apart; `engine_label` alone
    /// cannot do that (see the "Amended 2026-08-04" note on Task 17 in the v0.2 plan).
    pub profile_id: String,
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
pub(crate) struct RealProfileLoader {
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
    pub(crate) fn new(
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

        // Computed once and used to decide *both* whether refinement runs
        // for this profile *and* whether a refiner is even built — a
        // profile with refinement switched off (globally or by its own
        // policy) must never pay for one. Building it anyway would be
        // silently harmless at dispatch time (nothing calls a refiner
        // `refine_enabled` says to skip), but it is not free to construct:
        // `build_refiner` does a blocking keyring/DBus round trip for the
        // API key and hands back an HTTP client whose construction path
        // `expect`s — real cost, and a real (if inert) panic surface, on
        // the lazy-load path for a profile that will never use either.
        let refine_enabled = refinement_is_on(&self.global_refine, &profile.refine);
        let refiner: Option<Arc<dyn TextRefiner>> = if refine_enabled {
            let (refiner, refiner_notice) =
                build_refiner(&self.global_refine, self.dictionary_terms.clone());
            if let Some(msg) = refiner_notice {
                notices.push(("info", msg));
            }
            refiner.map(Arc::from)
        } else {
            None
        };

        let deps = ProfileDeps {
            engine,
            refiner,
            refine_enabled,
            tone: profile.refine.tone,
            language: Some(profile.language.clone()),
            engine_label: engine_label(profile.engine.active).to_string(),
            profile_id: profile.id.clone(),
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
///
/// **Holds a settings snapshot, not a live view.** [`RealProfileLoader`]
/// captures `global_refine` and `dictionary_terms` at construction, and
/// every [`Entry`] caches its [`ProfileDeps`] forever once loaded — there is
/// no `reload`/`invalidate` here. A settings change (the tray's refine
/// checkbox, an edited dictionary term, ...) has no effect on an
/// already-loaded profile until the whole registry is discarded and
/// rebuilt, and rebuilding throws away *every* lazily-loaded engine —
/// hundreds of MB, potentially — and re-pays the eager default-profile
/// load. `runtime_boot::build_deps` is where that recreate happens and
/// documents the decision to accept it (Task 16 of the v0.2 plan): parity
/// with the pre-profiles boot path, bounded by the same laziness this type
/// already provides.
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
    ///
    /// An empty `profiles` list produces a registry where every `deps_for`
    /// call returns `None` — no hotkey would ever dictate, and silently, so
    /// that case returns a `"warning"` notice instead of the usual empty
    /// list. Reachable: a hand-edited config with `profiles = []` parses to
    /// an empty `Vec` and is not caught by the v0.1 migration check (which
    /// only fires when the `profiles` key is absent, not when it's empty).
    ///
    /// This is not the *only* route to the same dead end, and the registry
    /// cannot see the other one: [`LanguageProfile::hotkey`] is a free-form
    /// string nothing validates when settings are loaded. A single profile
    /// with an unparseable chord (`""`, `"ctrl+"`, a typo'd key name) still
    /// makes `new` return a non-empty, notice-free registry — every
    /// `deps_for` call on it would succeed — but if the caller building the
    /// hotkey source (Task 16) drops chords `parse_hotkey` rejects, that
    /// profile's binding is never registered and its hotkey does nothing,
    /// silently. Only the caller doing that parsing can catch it; this
    /// module only ever sees `LanguageProfile`s, never their hotkey strings'
    /// validity. `runtime_boot::parse_profile_hotkeys` is the caller that
    /// does this parsing (Task 16 of the v0.2 plan) and reports the notice.
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
            vec![(
                "warning",
                "no language profiles configured; dictation has no hotkey until at least one \
                 profile is configured"
                    .to_string(),
            )]
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
    pub(crate) fn deps_for(
        &mut self,
        id: BindingId,
    ) -> Option<(&mut ProfileDeps, Vec<QueuedNotice>)> {
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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use utter_core::{SttError, TranscribeOptions, Transcript};
    use utter_store::profile::LanguageProfile;

    use super::*;
    use crate::runtime_boot::unavailable_engine;

    fn profile(id: &str) -> LanguageProfile {
        LanguageProfile {
            id: id.to_string(),
            ..LanguageProfile::default()
        }
    }

    /// An `SttEngine` that behaves like a real, working engine, as opposed
    /// to `unavailable_engine` (which always errors). Lets a test tell "this
    /// profile's engine is the genuine, healthy one" apart from "this
    /// profile's engine was silently swapped for the unavailable stand-in" —
    /// something asserting on `engine_label` alone cannot do, since a
    /// mutation can replace `deps.engine` without touching `engine_label`.
    struct HealthyEngine;

    impl SttEngine for HealthyEngine {
        fn begin(&mut self, _opts: &TranscribeOptions) -> Result<(), SttError> {
            Ok(())
        }

        fn feed(&mut self, _samples: &[i16]) -> Result<Option<String>, SttError> {
            Ok(None)
        }

        fn finish(&mut self) -> Result<Transcript, SttError> {
            Ok(Transcript {
                text: String::new(),
                language: None,
            })
        }
    }

    /// A `ProfileDeps` stand-in cheap to build repeatedly, carrying a
    /// genuinely healthy [`HealthyEngine`]. `engine_label` (and `profile_id`,
    /// which every caller here also sets to the profile's own id) is stamped
    /// with the profile's own id, so tests can assert a `deps_for` call
    /// resolved to the *right* profile rather than merely `Some` profile —
    /// see `a_profile_loads_its_engines_only_on_first_use` and
    /// `a_broken_profile_does_not_disable_the_others` below.
    fn fake_deps(engine_label: &str) -> ProfileDeps {
        ProfileDeps {
            engine: Box::new(HealthyEngine),
            refiner: None,
            refine_enabled: false,
            tone: Tone::Clean,
            language: None,
            engine_label: engine_label.to_string(),
            profile_id: engine_label.to_string(),
            dictionary_terms: Vec::new(),
        }
    }

    /// A loader that counts how many times it was asked to build a profile,
    /// so laziness can be pinned as a number rather than inferred from
    /// timing. Always succeeds, stamping each profile's own id as its
    /// `engine_label` (see `fake_deps`).
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

    /// A loader where the profile named `"default"`, and any profile whose
    /// id starts with `"broken"`, always fails to produce a real engine
    /// (mirroring a missing/damaged model), while every other profile loads
    /// cleanly and is stamped with its own id as `engine_label` (see
    /// `fake_deps`). Two distinct failure sites on purpose: `"default"`
    /// sits at index 0, which `ProfileRegistry::new` loads *eagerly*, so it
    /// pins a failure surfaced at boot; a `"broken"`-prefixed profile is
    /// placed at a non-zero index so its failure is only ever reached
    /// through a *lazy* `deps_for` load, pinning that the notice from that
    /// load reaches the caller of `deps_for` itself, not just `new`.
    struct FailingLoader;

    impl ProfileLoader for FailingLoader {
        fn load(&self, profile: &LanguageProfile) -> (ProfileDeps, Vec<QueuedNotice>) {
            if profile.id == "default" || profile.id.starts_with("broken") {
                let reason = format!("{}'s model is not downloaded", profile.id);
                let mut deps = fake_deps(&profile.id);
                deps.engine = unavailable_engine(reason.clone());
                (deps, vec![("warning", reason)])
            } else {
                (fake_deps(&profile.id), Vec::new())
            }
        }
    }

    #[test]
    fn a_profile_loads_its_engines_only_on_first_use() {
        let mut registry = test_registry_with_counting_loader();
        assert_eq!(
            registry.load_count(),
            1,
            "only the default profile loads at boot"
        );

        let (deps, _) = registry
            .deps_for(BindingId::from(1))
            .expect("binding exists");
        assert_eq!(
            deps.engine_label, "english",
            "binding 1 must resolve to profiles[1], not to whatever was loaded at boot"
        );
        assert_eq!(registry.load_count(), 2);

        registry.deps_for(BindingId::from(1));
        assert_eq!(registry.load_count(), 2, "a loaded profile is not rebuilt");
    }

    /// The profile at binding 0 (`"default"`) is the one `FailingLoader`
    /// fails eagerly, so `ProfileRegistry::new`'s eager load fails
    /// immediately — pinning the case where a fresh install's own default
    /// profile has a missing model, not just some other profile the user
    /// hasn't touched yet. Binding 1 (`"russian"`) is healthy and only
    /// loaded *after* that failure, checked for its own correct identity
    /// and a working engine — not merely for "some engine or other". Binding
    /// 2 (`"broken-german"`) is a *second*, independently broken profile at
    /// a non-zero index, only ever reached through a lazy `deps_for` call —
    /// its first press must carry the notice, and its second must not
    /// repeat it, pinning the property that justifies `deps_for` returning
    /// notices alongside the deps in the first place: a lazy load's failure
    /// has to reach whoever called `deps_for` at press time, not just
    /// whoever called `new` at boot.
    #[test]
    fn a_broken_profile_does_not_disable_the_others() {
        let profiles = vec![
            profile("default"),
            profile("russian"),
            profile("broken-german"),
        ];
        let (mut registry, boot_notices) = ProfileRegistry::new(profiles, Box::new(FailingLoader));

        assert!(
            !boot_notices.is_empty(),
            "a default profile that fails to load at boot must say so"
        );

        // Its own binding still resolves -- `None` would mean "unknown
        // binding", not "failed to load" -- and carries no further notice
        // since the one from `new` already covered it. Its engine is the
        // real `unavailable_engine` stand-in: `begin` errors.
        let (broken, notices) = registry
            .deps_for(BindingId::from(0))
            .expect("binding exists even though its load failed");
        assert_eq!(
            broken.engine_label, "default",
            "binding 0 must resolve to its own (broken) profile, not be swapped for another"
        );
        assert!(
            notices.is_empty(),
            "the boot-time notice must not repeat on every press"
        );
        assert!(
            broken.engine.begin(&TranscribeOptions::default()).is_err(),
            "the broken profile's engine is genuinely the unavailable stand-in"
        );

        // Loaded *after* the failure: must come back healthy, correctly
        // identified, AND with an engine that actually works -- not just an
        // unrelated field left untouched. This pair of assertions is what
        // actually distinguishes isolation from a registry that poisons
        // itself on any failure: the review's mutation keeps a `poisoned`
        // flag and, on every subsequent `deps_for`/`ensure_loaded`, silently
        // overwrites `deps.engine` with the unavailable stand-in -- without
        // ever touching `engine_label`, `deps_for`'s `Option`-ness, or its
        // notices. Checking only `engine_label`/notices (as an earlier
        // version of this test did) leaves that mutation green; the engine
        // itself has to be exercised.
        let (russian, notices) = registry
            .deps_for(BindingId::from(1))
            .expect("a broken default profile must not take the russian profile down with it");
        assert!(notices.is_empty(), "the healthy profile loads cleanly");
        assert_eq!(
            russian.engine_label, "russian",
            "binding 1 must resolve to the russian profile"
        );
        assert!(
            russian.engine.begin(&TranscribeOptions::default()).is_ok(),
            "the healthy profile's engine must actually work, not be silently degraded"
        );

        // Binding 2's own failure is *lazy*: nothing about it has been
        // loaded or reported before this first `deps_for` call, unlike
        // binding 0's, which `new` already surfaced. This is the case C3
        // pins: a `deps_for` that silently drops the load's notices (e.g.
        // `Some((deps, Vec::new()))` regardless of what the load produced)
        // passed every other assertion in this suite before this was added.
        let (broken_german, notices) = registry
            .deps_for(BindingId::from(2))
            .expect("binding exists even though its load will fail");
        assert!(
            !notices.is_empty(),
            "a lazy load that fails must say so on the call that triggered it"
        );
        assert_eq!(notices[0].0, "warning");
        assert_eq!(
            broken_german.engine_label, "broken-german",
            "binding 2 must resolve to its own (broken) profile, not be swapped for another"
        );
        assert!(
            broken_german
                .engine
                .begin(&TranscribeOptions::default())
                .is_err(),
            "binding 2's engine is genuinely the unavailable stand-in"
        );

        // Second press of the same binding: the notice already surfaced
        // once and must not repeat.
        let (_, notices) = registry
            .deps_for(BindingId::from(2))
            .expect("binding exists");
        assert!(
            notices.is_empty(),
            "a lazily-loaded profile's notice must not repeat on every subsequent press"
        );
    }

    #[test]
    fn an_unknown_binding_resolves_to_nothing() {
        let mut registry = test_registry_with_counting_loader();
        assert!(registry.deps_for(BindingId::from(99)).is_none());
        assert!(
            registry.deps_for(BindingId::from(2)).is_none(),
            "one past the end (a two-profile registry has no binding 2) is out of range too"
        );
    }

    /// Builds a registry from an ordered list and checks every binding
    /// resolves to the profile at its own position, not merely to "a"
    /// profile. `ProfileRegistry` documents that binding ids line up
    /// positionally with the `profiles` list it was given (matching how
    /// `utter_inject::create_source` assigns `BindingId`s); nothing else in
    /// this module verifies that alignment actually holds end to end.
    #[test]
    fn a_binding_resolves_to_the_profile_at_its_position() {
        let ids = ["default", "russian", "german", "french"];
        let profiles: Vec<LanguageProfile> = ids.iter().map(|id| profile(id)).collect();
        let count = Arc::new(AtomicUsize::new(0));
        let loader = Box::new(CountingLoader { count });
        let (mut registry, _notices) = ProfileRegistry::new(profiles, loader);

        for (index, id) in ids.iter().enumerate() {
            let (deps, _) = registry
                .deps_for(BindingId::from(index))
                .unwrap_or_else(|| panic!("binding {index} exists"));
            assert_eq!(
                &deps.engine_label, id,
                "binding {index} must resolve to profiles[{index}] (\"{id}\")"
            );
        }
    }

    #[test]
    fn an_empty_profile_list_warns_instead_of_silently_dictating_nothing() {
        let count = Arc::new(AtomicUsize::new(0));
        let loader = Box::new(CountingLoader { count });
        let (registry, notices) = ProfileRegistry::new(Vec::new(), loader);

        assert!(
            !notices.is_empty(),
            "an empty profile list must produce a notice, not silence"
        );
        assert_eq!(notices[0].0, "warning");
        assert_eq!(registry.entries.len(), 0);
    }

    /// `RealProfileLoader` itself: everything above exercises `ProfileLoader`
    /// through fakes, so nothing pins what the *production* loader actually
    /// does with a profile. This is the case the I1 fix (compute
    /// `refinement_is_on` once, skip `build_refiner` entirely when it's
    /// false) and the per-profile `language`/`tone` fields need: refinement
    /// switched on globally but off for this profile must build no refiner
    /// at all, and the profile's own language/tone must survive into
    /// `ProfileDeps` rather than being lost to some global default.
    ///
    /// Uses a nonexistent model directory (`build_engine` degrades to
    /// `unavailable_engine` + a notice with no models on disk, no network)
    /// and refinement switched *off* for this profile specifically, which
    /// never reaches `build_refiner` and therefore never touches the
    /// keyring either -- so this test needs neither models nor a keyring.
    #[test]
    fn a_profile_with_refinement_off_builds_no_refiner_and_keeps_its_own_language_and_tone() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg {
                enabled: true, // the global master switch is ON
                ..RefineCfg::default()
            },
            Vec::new(),
        );

        let mut profile = LanguageProfile {
            language: "ru".to_string(),
            ..LanguageProfile::default()
        };
        profile.refine.enabled = false; // this profile's own policy is OFF
        let tone = profile.refine.tone;

        let (deps, _notices) = loader.load(&profile);

        assert!(
            !deps.refine_enabled,
            "global on + profile off must not enable refinement"
        );
        assert!(
            deps.refiner.is_none(),
            "a profile with refinement off must not pay for a refiner -- no keyring round trip, \
             no HTTP client, no missing-key notice for a profile that will never refine"
        );
        assert_eq!(
            deps.language.as_deref(),
            Some("ru"),
            "the profile's own language must reach ProfileDeps, not be lost to auto-detect"
        );
        assert_eq!(
            deps.tone, tone,
            "the profile's own tone must reach ProfileDeps"
        );
        assert_eq!(
            deps.profile_id, "default",
            "the profile's own id must reach ProfileDeps, not be left blank"
        );
    }
}
