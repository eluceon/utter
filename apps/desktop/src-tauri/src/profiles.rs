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
//! reuses `build_sherpa`) and `runtime_boot::build_draft_engine` rather than
//! reimplementing engine construction, so that check is never bypassed by a
//! profile-specific load path — for the preview model just as much as for the
//! one whose text gets injected.

use std::sync::Arc;

use utter_core::{SttEngine, TextRefiner, Tone};
use utter_inject::BindingId;
use utter_store::settings::{refinement_is_on, RefineCfg};
use utter_store::{LanguageProfile, ModelManager};

use crate::runtime_boot::{
    build_draft_engine, build_engine, build_refiner, engine_label, QueuedNotice,
};

/// The per-profile slice of what a dictation session needs: everything
/// [`crate::runtime::RuntimeDeps`] does *not* already own once and share
/// across every profile (the hotkey receiver, history connection, capture
/// backend, injector, ...). Kept as its own type rather than folded into a
/// full `RuntimeDeps` per profile: those shared pieces exist exactly once
/// for the whole runtime and would have nowhere sensible to live if
/// duplicated per profile.
pub struct ProfileDeps {
    pub engine: Box<dyn SttEngine>,
    /// Optional streaming engine fed the same frames as `engine`, whose
    /// partials drive the HUD preview while the user is still speaking. Its
    /// text never contributes to what gets injected or recorded (spec D9) —
    /// `crate::runtime` never calls `finish()` on it, so there is no code
    /// path where its output could be mistaken for a transcript.
    ///
    /// Per-profile rather than per-runtime because a preview model is
    /// language-specific exactly like the profile's own engine: one global
    /// draft engine would show Russian speech as garbled English.
    ///
    /// `None` disables the preview for the profile — the default, and the
    /// state of any profile whose [`LanguageProfile::draft`] is unset or
    /// whose preview model could not be built. The runtime treats `None` as
    /// "show whatever partial the final engine itself produces", i.e.
    /// nothing at all for the offline engines in the catalog.
    pub draft_engine: Option<Box<dyn SttEngine>>,
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
    /// cannot do that.
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
/// worker thread, so whatever it holds must be movable there.
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

        // `"info"`, not the `"warning"` the final engine's failure earns: a
        // preview that cannot be built costs the user no transcript, only
        // the preview itself, and this profile keeps dictating normally with
        // `draft_engine: None`. `build_refiner`'s degradation is `"info"`
        // for the same reason.
        let (draft_engine, draft_notice) =
            build_draft_engine(profile.draft.as_ref(), &self.models, &self.dictionary_terms);
        if let Some(msg) = draft_notice {
            notices.push(("info", msg));
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
            draft_engine,
            refiner,
            refine_enabled,
            tone: profile.refine.tone,
            // A blank language (the shape `Profiles.svelte`'s free `TextInput` produces for a
            // newly added profile, or one the user simply cleared) must reach the engine as
            // `None`, its auto-detect value -- not as `Some("")`, which `cloud.rs` sends
            // verbatim as a rejected empty form field and `whisper.rs` treats as a real
            // (nonsensical) language rather than the `None`-means-auto path.
            language: (!profile.language.trim().is_empty()).then(|| profile.language.clone()),
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
/// given, the same order their chords are registered in via
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
/// documents the decision to accept it: parity with the pre-profiles boot
/// path, bounded by the same laziness this type already provides.
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
    /// hotkey source drops chords `parse_hotkey` rejects, that profile's
    /// binding is never registered and its hotkey does nothing, silently.
    /// Only the caller doing that parsing can catch it; this module only
    /// ever sees `LanguageProfile`s, never their hotkey strings' validity.
    /// `runtime_boot::parse_profile_hotkeys` is the caller that does this
    /// parsing and reports the notice.
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
    use utter_store::profile::{DraftCfg, LanguageProfile};

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
            draft_engine: None,
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
            id: "ru".to_string(),
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
            deps.profile_id, "ru",
            "the profile's own id must reach ProfileDeps, not be hardcoded -- the profile's id \
             is deliberately not \"default\" here, since a fixture built from \
             `LanguageProfile::default()` (whose id is literally \"default\") cannot tell a \
             copied id from a hardcoded string"
        );
    }

    /// The production loader must actually *read* `profile.draft`. With no models on disk the
    /// draft engine ends up `None` either way, so `draft_engine.is_none()` proves nothing at all
    /// here -- the notice is what distinguishes "the loader tried to build the configured preview
    /// model and found it missing" from "the loader never looked at `profile.draft`". Both
    /// directions are asserted in one test on purpose: a profile with no preview configured must
    /// stay silent, or every single-language user would be told at boot about a preview they
    /// never asked for.
    ///
    /// Deliberately build-agnostic: what the notice *says* differs between a build with the
    /// `sherpa` feature (the model is looked up and found missing) and one without it (no
    /// streaming engine exists to load it into at all), so the wording of each is pinned by its
    /// own `cfg`-gated test below, and this one asserts only what must hold in both.
    #[test]
    fn a_configured_preview_model_is_looked_up_and_a_missing_one_only_costs_the_preview() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let off = LanguageProfile::default();
        assert_eq!(
            off.draft, None,
            "fixture check: this profile really has no preview configured"
        );
        let (deps, notices) = loader.load(&off);
        assert!(
            deps.draft_engine.is_none(),
            "no preview configured, none built"
        );
        assert!(
            !notices.iter().any(|(_, msg)| msg.contains("preview")),
            "a profile with the preview off must say nothing about it, got {notices:?}"
        );

        let on = LanguageProfile {
            draft: Some(DraftCfg {
                model: "zipformer-ru-small".to_string(),
            }),
            ..LanguageProfile::default()
        };
        let (deps, notices) = loader.load(&on);
        assert!(
            deps.draft_engine.is_none(),
            "a preview model that is not downloaded leaves the preview off"
        );

        let (kind, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("zipformer-ru-small"))
            .expect("a configured but undownloaded preview model must be reported by name");
        assert_eq!(
            *kind, "info",
            "a preview that cannot be built costs no transcript, so it is not a warning"
        );
        assert!(
            msg.contains("Settings > "),
            "the notice must point at a Settings page, got {msg:?}"
        );
    }

    /// The wording of the missing-preview-model notice on a build that *has* the streaming
    /// engine: it must send the user to the one page that can fix it (Engines, where the model
    /// is downloaded) and say plainly that dictation itself still works.
    #[cfg(feature = "sherpa")]
    #[test]
    fn an_undownloaded_preview_model_points_at_the_engines_page() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "zipformer-ru-small".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (_deps, notices) = loader.load(&profile);
        let (_, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("zipformer-ru-small"))
            .expect("a configured but undownloaded preview model must be reported by name");

        assert!(msg.contains("not downloaded"), "got {msg:?}");
        assert!(msg.contains("Settings > Engines"), "got {msg:?}");
        assert!(
            msg.contains("only the live preview is off"),
            "the notice must say dictation is unaffected, got {msg:?}"
        );
    }

    /// The same profile on a build compiled without the `sherpa` feature: there is no streaming
    /// engine to load the model into, so the notice must send the user to Profiles (where the
    /// preview is switched off) rather than to Engines, which could not help.
    #[cfg(not(feature = "sherpa"))]
    #[test]
    fn a_preview_model_on_a_build_without_sherpa_points_at_the_profiles_page() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "zipformer-ru-small".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (_deps, notices) = loader.load(&profile);
        let (_, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("zipformer-ru-small"))
            .expect("a configured preview model must be reported even on a build without sherpa");

        assert!(msg.contains("without sherpa support"), "got {msg:?}");
        assert!(msg.contains("Settings > Profiles"), "got {msg:?}");
        assert!(
            msg.contains("only the live preview is off"),
            "the notice must say dictation is unaffected, got {msg:?}"
        );
    }

    /// A preview model configured as a blank id is the off state too (the shape a hand-edited
    /// config produces; the Profiles page writes `null`), and must be as silent as `None` --
    /// not reported as a missing model the user never selected.
    #[test]
    fn a_blank_preview_model_id_is_the_off_state_and_is_silent() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "   ".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (deps, notices) = loader.load(&profile);

        assert!(deps.draft_engine.is_none());
        assert!(
            !notices.iter().any(|(_, msg)| msg.contains("preview")),
            "a blank preview model is a choice, not a degradation, got {notices:?}"
        );
    }

    /// The `_Exit()` guard, on the draft path this time: a damaged preview model (here a
    /// truncated `encoder.onnx` among correctly sized siblings) must be caught by
    /// `verify_installed` and never reach `SherpaStreamingEngine::load`, which would hand it to
    /// sherpa-onnx's C++ layer -- and that calls `_Exit()`, killing the app, every profile
    /// included. The notice has to name the artifact and say re-download, not "not downloaded":
    /// the files *are* there, they are just wrong.
    #[cfg(feature = "sherpa")]
    #[test]
    fn a_damaged_preview_model_is_caught_before_it_reaches_sherpa() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model_dir = dir.path().join("models").join("zipformer-ru-small");
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        // Sizes from the catalog entry (`crates/utter-store/src/models.rs`); only the encoder is
        // wrong, so nothing but the size check can tell this install from a healthy one.
        std::fs::write(model_dir.join("encoder.onnx"), b"truncated").expect("write encoder");
        std::fs::write(model_dir.join("decoder.onnx"), vec![0u8; 2_093_080])
            .expect("write decoder");
        std::fs::write(model_dir.join("joiner.onnx"), vec![0u8; 259_417]).expect("write joiner");
        std::fs::write(model_dir.join("tokens.txt"), vec![0u8; 6_388]).expect("write tokens");

        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(dir.path().to_path_buf())),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "zipformer-ru-small".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (deps, notices) = loader.load(&profile);

        assert!(
            deps.draft_engine.is_none(),
            "a damaged preview model must leave the preview off, not load"
        );

        let (kind, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("zipformer-ru-small"))
            .expect("a damaged preview model must be reported");
        assert_eq!(*kind, "info");
        assert!(msg.contains("damaged"), "got {msg:?}");
        assert!(
            msg.contains("encoder.onnx"),
            "the notice must name the offending artifact, got {msg:?}"
        );
        assert!(msg.contains("re-download"), "got {msg:?}");
    }

    /// The route the `verify_installed` guard alone does not close: a profile whose preview
    /// model names an *offline* catalog entry. `parakeet-tdt-110m-en` installs under exactly the
    /// four artifact names `SherpaStreamingEngine::load` resolves, so an intact copy of it passes
    /// `verify_installed`, passes every file check, and reaches sherpa-onnx, which kills the
    /// process on the streaming metadata an offline export does not carry. The id's kind has to
    /// be settled from the catalog before any of that.
    ///
    /// The fixture installs `parakeet-tdt-110m-en` at the wrong sizes on purpose: without the
    /// kind check, `verify_installed` would call it damaged and the profile would still degrade
    /// politely, so a test that only asserted "preview off, some notice" would stay green. The
    /// notice having to name the model's *kind*, and not mention damage, is what pins the check
    /// running first and on its own grounds. (Installing it at the *right* sizes would be the
    /// purest fixture and is not worth 456 MB in a unit test; this ordering assertion covers the
    /// same defect.)
    #[cfg(feature = "sherpa")]
    #[test]
    fn an_offline_model_selected_as_the_preview_is_rejected_on_its_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let model_dir = dir.path().join("models").join("parakeet-tdt-110m-en");
        std::fs::create_dir_all(&model_dir).expect("create model dir");
        for name in ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"] {
            std::fs::write(model_dir.join(name), b"wrong size on purpose").expect("write artifact");
        }

        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(dir.path().to_path_buf())),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "parakeet-tdt-110m-en".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (deps, notices) = loader.load(&profile);

        assert!(
            deps.draft_engine.is_none(),
            "an offline model must never be loaded as a preview engine"
        );

        let (kind, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("parakeet-tdt-110m-en"))
            .expect("a preview model of the wrong kind must be reported by name");
        assert_eq!(
            *kind, "info",
            "the preview still degrades softly: dictation is unaffected"
        );
        assert!(
            msg.contains("an offline transcription model"),
            "the notice must say what the model actually is, got {msg:?}"
        );
        assert!(
            msg.contains("a streaming preview model"),
            "the notice must say what a preview needs instead, got {msg:?}"
        );
        assert!(
            msg.contains("Settings > Profiles"),
            "the notice must name the page where the preview model is chosen, got {msg:?}"
        );
        assert!(
            !msg.contains("damaged"),
            "the kind check must run before the integrity check, got {msg:?}"
        );
    }

    /// A preview model id that is in no catalog entry at all -- the third case, distinct from the
    /// wrong-kind one above. Reported as uncatalogued rather than as merely undownloaded, since
    /// no download could ever produce it.
    #[cfg(feature = "sherpa")]
    #[test]
    fn an_uncatalogued_preview_model_id_is_reported_as_unknown() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            draft: Some(DraftCfg {
                model: "zipformer-xx-small".to_string(),
            }),
            ..LanguageProfile::default()
        };

        let (deps, notices) = loader.load(&profile);

        assert!(deps.draft_engine.is_none());
        let (kind, msg) = notices
            .iter()
            .find(|(_, msg)| msg.contains("zipformer-xx-small"))
            .expect("an uncatalogued preview model id must be reported by name");
        assert_eq!(*kind, "info");
        assert!(msg.contains("not in the model catalog"), "got {msg:?}");
        assert!(
            msg.contains("only the live preview is off"),
            "the notice must still say dictation is unaffected, got {msg:?}"
        );
    }

    /// A blank language -- the shape `Profiles.svelte`'s free `TextInput` produces for a newly
    /// added profile -- must reach `ProfileDeps` as `None` (auto-detect), not `Some("")`. The
    /// test above only ever exercises a real language tag and would not notice a missing
    /// normalisation step.
    #[test]
    fn a_blank_profile_language_reaches_deps_as_none() {
        let loader = RealProfileLoader::new(
            Arc::new(ModelManager::new(PathBuf::from("/nonexistent"))),
            RefineCfg::default(),
            Vec::new(),
        );

        let profile = LanguageProfile {
            language: String::new(),
            ..LanguageProfile::default()
        };

        let (deps, _notices) = loader.load(&profile);

        assert_eq!(
            deps.language, None,
            "a blank language must normalise to None (auto-detect), not Some(\"\")"
        );
    }
}
