# General Engineering Rules

These rules are mandatory defaults for all long-lived, formally delivered software projects unless a project-specific contract explicitly overrides them.

They exist to prevent a recurring class of engineering failures: acting on assumptions instead of the current repository, manufacturing compatibility obligations before release, duplicating existing mechanisms, over-engineering defensive paths, producing large amounts of low-value tests or documentation, allowing god files to grow, failing to explain critical invariants in code, and optimizing for visible activity instead of repository quality.

Project-specific rules may be stricter. A project-specific rule overrides this document only where the project explicitly says so.

---

# 1. Development authority and source of truth

## 1.1 Read the real current project before editing

Before changing code, re-read the actual current project state relevant to the task.

This includes, as applicable:

- source files being modified;
- owning interfaces and types;
- adjacent modules that participate in the same behavior;
- current specifications;
- current architecture documents;
- current build and runtime wiring;
- current schemas;
- current configuration;
- current routes;
- current persistence models;
- current test or validation policy;
- current project-specific engineering rules.

Conversation history, prior summaries, remembered APIs, historical implementation details, and assumptions from earlier work are not substitutes for the current repository.

Do not implement based on “I remember this module worked like this”.

The current project must be inspected again.

## 1.2 Current authority outranks development history

The implementation authority is:

1. the current repository;
2. current non-obsolete specifications;
3. current explicit project decisions;
4. current project-specific engineering contracts.

Historical commits record development history. They do not define the current product.

Do not restore or preserve a behavior merely because:

- an older commit had it;
- an old field existed before;
- an old route existed before;
- an old API shape existed before;
- an old migration handled it;
- an old document mentioned it.

Historical commits may be inspected to understand how the project evolved, but they must not override current authority.

## 1.3 Do not invent missing product or architecture decisions

If a required implementation decision cannot be determined from current authority, do not silently invent a reasonable default.

Examples include uncertainty about:

- product behavior;
- ownership;
- authority;
- lifecycle;
- state transitions;
- persistence semantics;
- compatibility requirements;
- migration behavior;
- public API behavior;
- security boundaries;
- user-visible rules;
- feature scope.

When the project owner has not already made the decision and current authority does not determine it, surface the gap instead of manufacturing a contract.

Do not reconstruct product intent from historical code.

Do not convert an engineering guess into a product decision.

## 1.4 Distinguish fact, inference, recommendation, and unknown

Engineering communication must distinguish:

### Confirmed fact

Verified from current code, current documentation, actual runtime evidence, or explicit project decisions.

### Reasonable inference

A conclusion supported by current evidence but not explicitly established.

It must be described as an inference, not as repository fact.

### Recommendation

A design or engineering judgment about what should be done.

It must not be presented as existing project behavior.

### Unknown

Information that cannot currently be verified.

Do not guess to make the answer look complete.

---

# 2. Execution environment constraints

## 2.1 Never assume unavailable capabilities exist

Before relying on tools or environment capabilities, inspect what actually exists.

Do not assume the environment has:

- network access;
- GitHub access;
- a repository clone;
- Rust;
- Cargo;
- Node.js;
- pnpm;
- system package managers;
- language servers;
- compilers;
- build tools;
- Docker;
- databases;
- browsers;
- credentials;
- deployment access.

Use only capabilities actually available in the current execution environment.

## 2.2 No pointless network attempts in network-restricted environments

When the current environment does not provide ordinary outbound network access, do not repeatedly attempt operations that require it.

This includes:

- `git clone`;
- fetching Git dependencies;
- downloading release archives;
- `rustup`;
- `cargo install`;
- downloading toolchains;
- downloading package managers;
- downloading compilers;
- online bootstrap scripts;
- package installation that requires external registries.

Do not waste execution attempts trying variants of the same impossible path.

If a required tool is absent and cannot be installed with the available capabilities, state the resulting validation limitation and continue only with work that can be performed correctly.

## 2.3 Inspect local state before trying to recreate it

Before cloning, downloading, initializing, or generating anything, check whether the repository, dependency, generated file, toolchain, cache, or configuration already exists locally.

Do not recreate something simply because recreating it is a common workflow elsewhere.

## 2.4 Adapt to the project's actual workflow

Do not force a default development workflow onto the project.

Follow the project's actual:

- branch strategy;
- package manager;
- build system;
- formatter;
- linter;
- test policy;
- deployment model;
- code organization;
- documentation layout;
- release workflow.

The project should not be reshaped merely to fit a generic assistant workflow.

---

# 3. Product planning and engineering execution

## 3.1 Product decisions come before implementation design

When the task is a product-design or product-planning question, reason first about:

- user need;
- product rule;
- user-visible behavior;
- state model;
- permissions;
- ownership;
- moderation or governance rules where applicable;
- error and edge-case behavior;
- lifecycle;
- product boundaries;
- what must not happen.

Do not immediately jump to:

- schema;
- database tables;
- queue design;
- API routes;
- classes;
- worker topology;
- caches;
- storage engines.

Implementation design follows the product contract.

It does not replace it.

## 3.2 Do not silently downgrade formal projects into prototypes

Unless a project explicitly says otherwise, assume the project is intended for:

- formal delivery;
- long-term maintenance;
- continued iteration;
- future contributors;
- production-quality architecture.

Do not silently reframe the target as:

- prototype;
- demo;
- MVP;
- proof of concept;
- temporary implementation;
- throwaway experiment.

This does not justify over-engineering.

It means the current solution must have coherent ownership, maintainable boundaries, and a credible path for continued development.

## 3.3 “Temporary” is not an excuse for structural debt

Do not introduce knowingly poor architecture under labels such as:

- temporary;
- for now;
- later cleanup;
- MVP-only;
- compatibility for now;
- quick workaround.

If a truly temporary mechanism is necessary, its scope and removal condition must be explicit.

---

# 4. Pre-release compatibility policy

## 4.1 Development history is not a compatibility obligation

Before a project has externally distributed a contract, or explicitly frozen a format/API/ABI/schema for a real integration, development-era contracts are not compatibility obligations.

Do not preserve obsolete development state merely because it once existed.

## 4.2 Keep one current correct shape before compatibility becomes real

For unpublished or unfrozen contracts, prefer one current correct representation.

When an incomplete or incorrect development design is replaced:

- remove obsolete fields;
- remove obsolete routes;
- remove obsolete API aliases;
- remove obsolete schema shapes;
- remove obsolete decoders;
- remove obsolete compatibility modes;
- remove obsolete adapters;
- remove obsolete fixtures;
- remove obsolete scaffolding;
- remove obsolete development-only state.

Do not retain parallel old and new shapes without a real compatibility requirement.

## 4.3 Do not create migrations for imaginary production data

Do not add migrations merely to preserve development data that has never been part of a released system.

Do not manufacture migration infrastructure to protect:

- local scratch data;
- disposable development databases;
- unpublished schemas;
- abandoned development formats;
- historical test fixtures.

A migration is justified when there is actual data or an explicitly frozen contract that must survive a schema change.

## 4.4 Git history preserves development history

Do not encode every intermediate development design into runtime compatibility branches.

Git already preserves historical implementation.

The runtime should preserve supported contracts, not archaeological layers.

## 4.5 Pre-release freedom does not weaken architecture invariants

Being pre-release does not justify violating:

- identity separation;
- authority boundaries;
- ownership rules;
- lifecycle correctness;
- security boundaries;
- fail-closed behavior;
- persistence correctness;
- client/server responsibility;
- resource safety.

Compatibility obligations may be absent.

Correctness obligations are not.

---

# 5. Reuse, canonical ownership, and duplicate policy

## 5.1 Search semantically before adding reusable mechanisms

Before adding or changing reusable behavior, search the whole current repository for the same semantics or invariant.

Do not search only for the function name you intend to create.

Search for the rule itself.

Relevant categories commonly include:

- parsing;
- validation;
- serialization;
- encoding and decoding;
- stable identifiers;
- path rules;
- authorization;
- capability checks;
- synchronization;
- retries;
- rate limiting;
- resource tracking;
- time/deadline handling;
- overflow behavior;
- persistence validation;
- conversion;
- materialization;
- state transition rules;
- identity rules;
- normalization;
- portability behavior.

## 5.2 One rule should have one canonical owner

When several call sites need the same policy, identify the narrowest correct owner.

The actual implementation of the rule belongs there.

Other consumers delegate to it.

Do not copy the rule into each subsystem because copying is locally convenient.

## 5.3 Do not create parallel policy implementations

Two implementations of the same invariant create drift.

If the same rule exists twice:

- identify the canonical owner;
- remove the duplicate body;
- delegate to the owner;
- split responsibilities if the apparent duplication reveals an ownership problem.

## 5.4 Reuse existing project abstractions before creating new ones

Before writing a new helper, service, parser, validator, wrapper, codec, state machine, or abstraction, inspect what the project already provides.

Do not locally reinvent functionality simply because writing a new version is faster than understanding the existing one.

Repository consistency is more important than local convenience.

## 5.5 Do not mechanically apply DRY

Similar-looking code is not automatically the same rule.

Keep code separate when it has genuinely different:

- authority owners;
- security owners;
- lifetimes;
- state owners;
- type semantics;
- persistence contracts;
- failure semantics.

Strong semantic boundaries outrank superficial textual similarity.

## 5.6 Do not create generic helper dumping grounds

Do not solve ownership uncertainty by creating catch-all modules such as:

- `utils`;
- `helpers`;
- `common`;
- `misc`;
- `shared` with no defined domain.

Shared code must still have a clear semantic owner.

If a helper has no obvious owner, the ownership problem must be solved rather than hidden in a dumping ground.

## 5.7 Thin semantic wrappers are acceptable when they preserve a real boundary

A type-specific or subsystem-specific wrapper may be useful even when it delegates to a shared implementation.

The wrapper is justified when its name and location preserve a meaningful domain boundary.

Its body should delegate the underlying common rule to the canonical owner.

---

# 6. Defensive programming policy

## 6.1 Defend real boundaries, not imaginary scenarios

Defensive programming must respond to a concrete failure model.

Valid defensive boundaries commonly include:

- untrusted external input;
- authorization;
- security;
- concurrency;
- ownership;
- lifetime;
- persistence;
- atomicity;
- resource exhaustion;
- network failure;
- external service failure;
- explicit platform constraints.

Do not add machinery solely because “something might theoretically go wrong”.

## 6.2 Do not manufacture defensive state

Avoid unnecessary:

- fallback branches;
- checkpoints;
- hashes;
- duplicate verification layers;
- shadow state;
- redundant state flags;
- compatibility state;
- repair paths;
- recovery modes;
- redundant guards;
- duplicated integrity checks.

Each such mechanism increases state-space complexity and maintenance cost.

It requires a real justification.

## 6.3 Do not confuse more checks with more correctness

A large number of checks can obscure the real invariant.

Prefer a small number of checks at the correct authority boundary.

Do not scatter the same validation throughout the system “for safety” when there should be one canonical validation owner.

## 6.4 Defensive code must have an owner and a failure model

Before adding a defensive mechanism, be able to answer:

- What exact failure does it protect against?
- Can that failure actually occur?
- At what boundary does it occur?
- Which subsystem owns that boundary?
- Why is this location the correct enforcement point?
- What happens if the mechanism is removed?
- Is the complexity proportional to the risk?

If those questions cannot be answered, the mechanism is probably unjustified.

---

# 7. Testing and validation policy

## 7.1 Test count is not a quality metric

Do not optimize for:

- number of tests;
- number of assertions;
- number of fixtures;
- number of checkpoints;
- number of smoke checks;
- line count of test code.

A large test suite with weak behavioral coverage is not better than a smaller suite covering the real risks.

## 7.2 Prioritize critical behavior over trivial testability

Testing effort should focus first on:

- core product paths;
- business invariants;
- permissions;
- security boundaries;
- state transitions;
- persistence behavior;
- cross-module behavior;
- network paths;
- lifecycle;
- startup;
- failure handling;
- shutdown;
- resource teardown;
- regression-prone behavior.

Do not spend disproportionate effort testing trivial helpers while critical workflows remain weakly covered.

## 7.3 Avoid implementation-detail testing unless the detail is itself a contract

Do not tightly bind tests to internal implementation choices without reason.

Prefer behavior and invariant verification.

Implementation-detail tests are justified when the implementation detail is itself a required contract, such as:

- ordering;
- atomicity;
- ownership;
- persistence layout explicitly declared stable;
- protocol behavior;
- security enforcement point.

## 7.4 Validate the real product path

A subsystem is not accepted merely because isolated pieces work.

Exercise the actual path the feature claims to support.

Depending on the project, this may include:

- real executable entry points;
- real request paths;
- real routing;
- real persistence;
- real runtime state;
- real authorization;
- real network/session paths;
- real module packages;
- real assets;
- real backend integrations;
- real cleanup behavior.

Synthetic checks supplement this evidence.

They do not replace it.

## 7.5 Lower-level checks cannot overrule a failing real path

If isolated tests pass but the real product path fails, the feature is not complete.

Do not present:

- green unit tests;
- a synthetic smoke workload;
- source scans;
- fixture success;
- check counts;

as proof that the end-to-end feature works.

## 7.6 Do not build hidden test suites under other names

Do not manufacture test infrastructure disguised as:

- validation helpers;
- checkpoint systems;
- diagnostic executables;
- smoke harnesses;
- hash verifiers;
- internal runners.

A validation helper is justified outside the test suite only when it is genuinely useful as long-lived product, developer, diagnostic, or operational tooling.

## 7.7 Match the project's actual test policy

Do not impose a test architecture the project has explicitly rejected.

Do not remove testing requirements the project has explicitly adopted.

Project-specific validation policy controls the exact tools.

The universal rule is that evidence must correspond to the real behavior being claimed.

---

# 8. Comments and implementation knowledge

## 8.1 Do not target a numeric comment percentage

Comment density is not a quality metric.

Do not add comments merely to increase a ratio.

Do not narrate obvious syntax.

## 8.2 Critical non-obvious invariants require local comments

A local comment is required when correctness depends on information that a maintainer cannot reliably infer from nearby code.

Examples include:

- ownership transfer;
- borrowed vs owned lifetime;
- destruction responsibility;
- lock scope;
- concurrency order;
- atomic publication;
- memory ordering;
- commit points;
- state transition restrictions;
- durability ordering;
- atomic replacement;
- fail-closed recovery;
- authority boundaries;
- identity selection;
- security intersections;
- sandbox boundaries;
- FFI boundaries;
- resource exhaustion limits;
- starvation control;
- deliberately separated similar implementations;
- compiler phase assumptions;
- lowering constraints;
- semantic invariants;
- protocol invariants;
- non-obvious error propagation behavior.

## 8.3 Good comments explain why and what breaks

A useful implementation comment should answer questions such as:

- Why does this check exist?
- Why must this order be preserved?
- Why is this ownership rule necessary?
- Why is this code intentionally separate from a similar implementation?
- What invariant is being protected?
- What would break if this seemingly simpler edit were made?

Comments should not merely translate the code into prose.

## 8.4 Specifications do not replace local invariant comments

A rule may already exist in a specification and still require a local comment.

If a locally plausible code edit could violate the rule, the implementation should explain the relevant invariant near the code.

A maintainer should not need to know that a distant Markdown file contains the reason a particular line must not be changed.

## 8.5 Comments are part of the implementation contract

When behavior changes:

- review adjacent comments;
- update changed rationale;
- remove stale comments;
- add newly required invariant explanations.

A stale comment is a correctness hazard.

---

# 9. Documentation policy

## 9.1 Do not use documentation as a substitute for readable code

Do not compensate for weak code structure by producing large amounts of Markdown.

Documentation must not become a dumping ground for implementation facts that should instead be expressed by:

- type design;
- module ownership;
- naming;
- local invariant comments;
- code structure.

## 9.2 Documentation should describe durable cross-cutting contracts

Long-lived documentation is appropriate for things such as:

- product rules;
- architecture;
- subsystem responsibilities;
- external APIs;
- protocols;
- stable schemas;
- data models;
- build and release contracts;
- deployment procedures;
- operational behavior;
- engineering policies.

Avoid creating documents for transient implementation details that are better kept with the code.

## 9.3 Code and documentation complete the same task

A feature, fix, refactor, or architecture change is not complete when it knowingly leaves affected documentation stale.

When code changes a documented fact, update that documentation in the same task.

Do not defer documentation synchronization to an unspecified future cleanup.

## 9.4 Delete obsolete documentation

Do not keep outdated documents merely because they contain historical information.

Git preserves history.

If a document no longer describes the current system and has no active archival purpose, update or remove it.

## 9.5 More documentation is not automatically better

Do not measure engineering quality by:

- number of Markdown files;
- number of design documents;
- length of documentation;
- amount of generated explanation.

Documentation has value only when it is current, authoritative, appropriately scoped, and useful to maintenance.

---

# 10. God-file and responsibility boundaries

## 10.1 A god file is defined by responsibility, not line count

A file becomes a god file when it owns multiple independently changing responsibilities that should have separate cohesion boundaries.

Size is a warning signal.

It is not the definition.

## 10.2 Strong god-file indicators

Review aggressively when one file owns combinations such as:

- unrelated lifecycle phases;
- routing and persistence;
- protocol framing and permissions;
- policy and backend integration;
- scheduling and serialization;
- infrastructure and many domain-specific implementations;
- multiple unrelated state machines;
- unrelated features editing the same central file repeatedly.

Also review documents that have become several independent specifications hidden in one Markdown file.

## 10.3 Do not keep adding to a central file because it is convenient

The fact that the current feature already touches a file does not justify adding another responsibility to it.

Before expanding a large or central file, ask whether the new behavior belongs to an existing cohesive responsibility or deserves a separate owner.

## 10.4 Split by responsibility, not arbitrary line ranges

Do not split a large file by:

- first half / second half;
- arbitrary line counts;
- alphabetical ranges;
- accidental local grouping.

Each resulting module should have a clear reason to change.

## 10.5 A split must improve ownership

After a split, the repository should have clearer:

- responsibility;
- dependencies;
- state ownership;
- API boundaries;
- lifecycle ownership.

A split that merely moves lines between files without clarifying ownership is not a successful refactor.

---

# 11. Dependency and API ownership

## 11.1 Depend explicitly on the owner of a contract

A module should obtain the declarations, types, functions, or contracts it uses from the actual owner.

Do not rely on accidental visibility through:

- transitive imports;
- unrelated aggregate modules;
- giant barrel exports;
- unrelated platform headers;
- implicit globals;
- unrelated re-exports.

## 11.2 Do not fix ownership mistakes with ad-hoc declarations or suppression

Do not patch an undeclared or inaccessible dependency by:

- duplicating a declaration;
- adding an ad-hoc prototype;
- adding an unrelated import;
- suppressing diagnostics;
- exporting an internal symbol globally without design review.

Fix the dependency relationship.

## 11.3 Internal reuse does not automatically justify public API

A helper or type does not become public merely because several internal consumers need it.

Public APIs are contracts.

They should exist because external or cross-boundary consumers are intentionally supported, not because making something public is an easy way to resolve imports.

## 11.4 Keep private implementation contracts private

Internal implementation interfaces should remain internal unless there is a deliberate reason to expose them.

Avoid accidental public surface-area growth.

Every new public API creates future compatibility and maintenance obligations.

---

# 12. Warning, lint, and diagnostic policy

## 12.1 Fix causes instead of hiding diagnostics

For project-owned code, warnings and meaningful lints should be treated as defects unless the project explicitly defines otherwise.

Prefer fixing the underlying cause.

Do not use broad suppression to make the build appear clean.

## 12.2 Suppression must be narrow and justified

When a warning or lint genuinely cannot be avoided:

- scope the suppression as narrowly as possible;
- explain why it is necessary;
- keep the explanation adjacent to the suppression.

Do not disable entire categories globally merely for convenience.

## 12.3 Third-party code and project-owned code are different

Do not spend project effort rewriting vendor code solely to satisfy project-owned style policy unless required.

At the same time, project-owned code compiled through vendor targets remains project-owned and must follow project rules.

---

# 13. Review to a fixed point

## 13.1 Review is iterative

One review pass is not sufficient when the review itself causes structural changes.

After a change, inspect again for:

- duplicate policy implementations;
- missing invariant comments;
- god files;
- stale documentation;
- unclear ownership;
- accidental public APIs;
- unnecessary compatibility code;
- unnecessary defensive mechanisms;
- warnings;
- validation gaps.

If fixes reveal new issues of the same class, continue.

## 13.2 A review reaches a fixed point when another pass finds no new same-class issue

Do not stop merely because the task has already received one cleanup pass.

The goal is a stable repository state for the scope touched by the change.

## 13.3 Review the owning subsystem, not only the exact edited lines

A local edit can expose structural problems in the surrounding subsystem.

Review enough context to determine whether the change:

- duplicates existing behavior;
- extends a god file;
- violates ownership;
- leaves stale documentation;
- adds compatibility debt;
- creates unnecessary test infrastructure.

---

# 14. Work quality and change discipline

## 14.1 Visible effort is not engineering quality

Do not optimize for producing more:

- code;
- tests;
- documentation;
- abstractions;
- validation layers;
- compatibility layers;
- commits;
- files.

Engineering quality is measured by whether the project becomes:

- more correct;
- simpler;
- more coherent;
- easier to maintain;
- easier to extend;
- easier to reason about;
- less duplicated;
- more explicit about invariants.

## 14.2 Prefer deleting obsolete complexity

A strong change may remove more code than it adds.

Do not treat deletion as lesser work.

Removing:

- dead compatibility code;
- duplicate helpers;
- obsolete fields;
- stale routes;
- redundant validators;
- unnecessary state;
- outdated documents;
- dead abstractions;

is often the correct engineering result.

## 14.3 Do not add infrastructure without a demonstrated need

Do not introduce a new:

- abstraction layer;
- framework;
- helper subsystem;
- checkpoint system;
- hashing layer;
- migration framework;
- test harness;
- caching layer;
- adapter;
- compatibility layer;

merely because it might be useful later.

Add infrastructure when the current project actually needs it and the owning contract is clear.

## 14.4 A completed task must not leave the repository structurally worse

Task-level success is insufficient.

A feature is not a good change if it technically works but:

- duplicates policy;
- grows a god file;
- adds unnecessary state;
- introduces stale documentation;
- creates an unclear owner;
- expands public API accidentally;
- adds unjustified compatibility;
- weakens maintainability.

Completion requires both behavioral correctness and acceptable repository structure.

---

# 15. Interaction with confirmed project decisions

## 15.1 Do not repeatedly reopen settled decisions

When the project owner has explicitly chosen a solution, do not keep reintroducing rejected alternatives unless new evidence creates a real conflict.

Do not repeatedly re-explain why the already-selected solution is reasonable.

Proceed with implementation.

## 15.2 Do not restate decisions instead of doing the work

Once instructions are clear, avoid filler such as:

- “We will now...”
- “The plan is...”
- repeating the user's decision in different words;
- summarizing already-established constraints without need.

Report actual results, actual discoveries, and actual decision gaps.

## 15.3 Do not manufacture objections the project owner did not make

Do not invent a hypothetical user belief and then answer it.

Respond to the actual question or actual design position.

If an inference is necessary, label it as the assistant's inference.

## 15.4 Do not agree automatically

Before accepting a technical or product claim, check:

- factual correctness;
- logical consistency;
- repository evidence;
- missing constraints;
- counterexamples;
- current project state.

If the claim is wrong or unsupported, say so and explain why.

Agreement is not a substitute for engineering review.

---

# 16. Repository history and Git workflow

## 16.1 Follow the project's branch and commit policy exactly

Do not invent a branch strategy.

Some projects may work directly on a primary branch.

Others may require pull requests.

Follow the active project's explicit policy.

## 16.2 Preserve real history unless the project explicitly requires rewriting it

Do not rewrite already-created history merely to make it look cleaner unless the project workflow explicitly calls for it.

Corrections should normally remain visible as real corrections.

## 16.3 Historical commits are not implementation authority

This rule applies regardless of branch strategy.

Do not resurrect historical behavior merely because it existed.

## 16.4 Pull-request branches are deleted after merge

For GitHub pull-request workflows, once a pull request is merged, delete the corresponding branch immediately.

Do not recommend retaining merged branches by default.

A merged branch is no longer an active development branch unless the project explicitly defines an exception.

---

# 17. Practical implementation workflow

The exact commands vary by project, but the universal sequence is:

```text
inspect the actual current environment
  -> inspect the current repository and active project rules
  -> read current source/spec/build/config relevant to the task
  -> identify product behavior and ownership
  -> search the repository semantically for existing implementations
  -> identify the canonical owner
  -> implement without unnecessary compatibility or defensive machinery
  -> add required local invariant comments
  -> update affected long-lived documentation
  -> review duplicate policies
  -> review ownership and public/private boundaries
  -> review god files
  -> review warnings/lints
  -> validate the real product path
  -> inspect actual runtime evidence where available
  -> repeat structural review until fixed point
  -> commit or submit using the project's actual Git workflow
  -> after a merged GitHub PR, delete its branch
```

---

# 18. Completion gates

A change is not complete merely because code was written.

For the changed scope, confirm all applicable items below.

## Current reality

- The implementation was based on the current repository, not memory.
- The actual execution environment was inspected.
- No unavailable network/tool capability was assumed.
- No historical commit was treated as current design authority.

## Product and architecture

- Product behavior is determined by current authority.
- No missing product decision was silently invented.
- Ownership is clear.
- Public/private boundaries are intentional.
- The solution is appropriate for a formally maintained project.

## Reuse and duplication

- The repository was searched semantically for existing owners.
- Existing mechanisms were reused where appropriate.
- No second implementation of the same policy was introduced.
- No generic helper dumping ground was created.
- Similar-but-separate implementations have a real semantic reason to remain separate.

## Compatibility

- No development-era compatibility was retained without a real requirement.
- No obsolete field/path/API/schema remains solely because it existed before.
- No migration was added for imaginary production data.
- Old development scaffolding was removed where appropriate.

## Defensive programming

- Defensive mechanisms correspond to real failure modes.
- No unjustified checkpoint/hash/fallback/shadow state was added.
- Validation lives at the correct authority boundary.
- Complexity is proportional to real risk.

## Tests and validation

- Critical product behavior has appropriate coverage.
- Test count was not treated as evidence by itself.
- The real product path was exercised where possible.
- Synthetic checks were not presented as end-to-end proof.
- No hidden test framework was introduced under another name.

## Comments

- Non-obvious correctness invariants are explained locally.
- Comments explain why and what breaks, not obvious syntax.
- Changed behavior triggered review of adjacent comments.
- Stale comments were removed or corrected.

## Documentation

- Long-lived documentation reflects the current implementation.
- No known stale documentation remains for the changed behavior.
- Documentation was not created merely to compensate for unclear code.
- Obsolete documents were updated or removed.

## Structure

- Changed files and owning subsystems were reviewed for god-file behavior.
- Responsibilities remain cohesive.
- Any split was performed by responsibility, not line count.
- The change did not make a central file a dumping ground.

## Diagnostics

- Project-owned warnings/lints were fixed rather than broadly suppressed.
- Any unavoidable suppression is narrow and documented.

## Final structural quality

- Another review pass does not reveal a new duplicate-policy issue.
- Another review pass does not reveal a new missing invariant comment.
- Another review pass does not reveal a new god-file problem.
- Another review pass does not reveal newly stale documentation.
- The repository is at least as coherent and maintainable as before the change.

---

# 19. Anti-pattern catalogue

The following patterns are explicitly disallowed unless a current project-specific requirement justifies them.

## Environment anti-patterns

- Repeatedly trying to clone a repository without usable network access.
- Trying to bootstrap Rust or another toolchain through unavailable network access.
- Assuming common tools are installed without checking.
- Recreating a local asset that already exists.

## Compatibility anti-patterns

- Keeping an obsolete field “just in case”.
- Keeping an obsolete route for nonexistent users.
- Preserving an old schema for unreleased data.
- Adding compatibility aliases for unpublished APIs.
- Writing migrations solely for disposable development databases.
- Carrying multiple development-era representations indefinitely.

## Defensive-programming anti-patterns

- Adding a checkpoint because it “feels safer”.
- Adding a hash without a concrete integrity threat.
- Maintaining shadow state without an ownership requirement.
- Adding duplicate validation at every layer.
- Adding fallback paths that hide real contract violations.
- Designing recovery for states the architecture should make impossible.

## Reuse anti-patterns

- Writing a new validator without searching for the existing validator.
- Reimplementing parsing because the existing parser is in another module.
- Copying a helper and changing its name.
- Creating `utils` because ownership is unclear.
- Merging semantically distinct code solely to remove textual duplication.

## Testing anti-patterns

- Measuring progress by test count.
- Testing every trivial helper while core workflows remain weak.
- Treating source scans as runtime proof.
- Treating synthetic fixtures as real integration evidence.
- Building custom checkpoint/hash infrastructure instead of testing the real behavior.
- Writing tests primarily to make the change look substantial.

## Comment anti-patterns

- Leaving complex ownership or state transitions unexplained.
- Moving implementation rationale into a distant Markdown file instead of adding a local invariant comment.
- Writing comments that merely narrate syntax.
- Leaving stale comments after behavior changes.

## Documentation anti-patterns

- Creating large volumes of Markdown instead of improving code structure.
- Updating code while knowingly leaving docs stale.
- Keeping obsolete specifications because deleting them feels destructive.
- Treating documentation quantity as a quality metric.

## Architecture anti-patterns

- Adding another unrelated responsibility to an already central file.
- Splitting a file by arbitrary line count.
- Exporting internal helpers publicly just to make imports easier.
- Relying on accidental transitive dependencies.
- Adding abstraction layers before there is a real owner or repeated need.

## Work-discipline anti-patterns

- Repeating already-confirmed decisions instead of executing them.
- Reopening rejected options without new evidence.
- Inventing a user assumption and arguing against it.
- Automatically agreeing before checking the repository or facts.
- Presenting inference as confirmed project state.
- Producing more code/tests/docs primarily to demonstrate effort.

---

# 20. Core engineering standard

The repository is the product of the work.

The goal is not to maximize visible activity.

The goal is to leave the project:

- correct;
- simpler where possible;
- coherent;
- explicit about ownership;
- explicit about non-obvious invariants;
- free of unnecessary compatibility debt;
- free of duplicate policy implementations;
- free of unjustified defensive machinery;
- validated through real behavior;
- documented accurately;
- maintainable over long-term iteration.

A change that produces more code, tests, documentation, compatibility paths, or abstractions but leaves the project harder to understand or maintain is not a successful change.

The preferred direction is:

> fewer owners, clearer contracts, fewer states, fewer duplicate rules, fewer stale artifacts, stronger invariants, and evidence from the real product path.
