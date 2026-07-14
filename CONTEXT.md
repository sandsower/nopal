# Nopal

Nopal is a trustworthy agent-management product that puts agents to work while keeping the operator's assurance inspectable.
It is delivered as a distribution over Pi and powered by one deterministic core with multiple thin surfaces.

## Language

**Nopal Core**:
The deterministic engine - gates, policy, run ledger, process artifacts, and authoritative workflow transitions; it decides, selects, and explains, and never executes.
It is the component previously named Olin, and it is the only place deterministic semantics live.
Beislið depends on Nopal Core without depending on the Nopal application or Pi distribution; that product relationship is intentional.
_Avoid_: Olin as the final component name, placing deterministic decisions in a host extension or skill

**Nopal**:
The final product name and outward identity, with the secured `nopal.sh` as its canonical public address.
It presents a trustworthy agent-management product rather than a Pi distribution or extension bundle.
_Avoid_: presenting Nopal as merely Pi with extra extensions

**Field**:
Nopal's main management surface and top-level view of Plots across Repositories and execution modes.
_Avoid_: cockpit, herd, dashboard

**Repository**:
The canonical source and policy scope whose root supplies configuration, Roots, and Spines for work performed against it.
_Avoid_: project, workspace, treating an organizational grouping as policy authority

**Workspace**:
An inspectable execution environment attached to a Repository, such as its primary checkout, a worktree, or an isolated runtime.
It owns mutable execution state but never policy, and remains secondary to the Plot in Nopal's outward language.
_Avoid_: project, repository, top-level navigation object, assuming conversation requires one

**Host**:
A system that mediates one or more workflow activities for a Plot, such as interactive agent work, unattended execution, approval, or verification.
A Host contributes only the enforcement guarantees it can prove and a Plot may rely on several Hosts.
_Avoid_: agent, model, properly equipped host, assuming product identity establishes trust

**Execution Mode**:
The operating posture of a Session or execution, either Interactive or Unattended.
A Plot or Subplot may use either or both without changing its Intent, Fruit, Progress, or assurance boundary.
_Avoid_: AFK Plot, HITL Plot, creating a Subplot only to change mode

**Session**:
An interactive agent conversation attached to one Plot and bound to one Workspace for its entire lifetime.
Work in another Workspace begins a new Session so conversation, execution state, and Evidence remain unambiguous.
_Avoid_: execution, run, moving an existing Session between Workspaces

**Plot**:
A bounded effort toward an intended outcome that Nopal plans, manages, and makes inspectable across one or more agents and execution attempts.
_Avoid_: assignment, brote, pad, penca, work item

**Provisional Plot**:
A Planned Plot with a stable identity and immutable Seed that has not yet been established against a Repository.
It follows a fixed minimal Workflow that permits conversation, discovery, and Establishment but no Repository-scoped mutation; it becomes an ordinary Plot in place when established and may expire if never established.
_Avoid_: draft chat, disposable Plot, recreating it as a different Plot when work begins

**Establishment**:
The deterministic, configured transition that binds a Provisional Plot to one or more Repositories and resolves its applicable Roots and Spines while preserving its prior history.
It records the effective Workflow, Roots, Proof Requirements, and Gate declarations that govern the established Plot.
_Avoid_: model judgment that work has become serious, hard-coding worktree creation as the only trigger

**Subplot**:
A Plot with its own Intent, Fruit, and Evidence whose accepted Fruit contributes to a parent Plot.
It is a single-level assurance boundary rather than an execution attempt or checklist item, and it can never contain another Subplot.
_Avoid_: subtask, child run, nested Subplot, treating independently acceptable work as an opaque part of its parent

**Subagent**:
An execution participant delegated work within one Plot or Subplot and operating under that Plot's Intent, Roots, and Workflow.
It may produce Evidence but has no independent Intent, Fruit, Progress, or assurance boundary.
_Avoid_: Subplot, automatically treating delegation as outcome decomposition

**Offshoot**:
An independent Plot arising from a Closed Plot for continuation, correction, or supersession.
It preserves lineage to its predecessor without reopening or rewriting the predecessor's Fruit, Roots, or Evidence.
_Avoid_: reopening a Closed Plot or treating later work as part of its historical record

**Seed**:
The immutable initiating brief, request, or ticket from which a Plot begins.
_Avoid_: editing the Seed to match what agents eventually produced

**Intent**:
The current approved outcome a Plot is pursuing.
Intent may evolve through recorded decisions, but its history and relationship to the Seed remain inspectable.
_Avoid_: presenting current Intent as though it were always the original request

**Progress**:
The current lifecycle position of a Plot, independent from its Fruit, Root satisfaction, Evidence coverage, and Exceptions.
Its phases are Planned, Active, Review, and Closed.
_Avoid_: treating completion, successful execution, or agent exit as assurance

**Condition**:
An optional circumstance affecting a Plot without changing its Progress phase; a Plot may have zero or several Conditions.
Operator attention is derived from each Condition's ownership and urgency rather than stored as a Condition itself.
_Avoid_: clear or healthy as a substitute for having no Conditions, or needs attention as a primary Condition

**Authority**:
A configured source empowered to make a proposal binding for a defined scope and decision type.
Authority may come from a human role, deterministic rule, or external attestation; agent output alone never constitutes Authority.
_Avoid_: authorship, agent confidence, assuming every user has every approval right

**Roots**:
The authoritative externalized requirements that govern a Plot independently of any agent's context or claims.
Every Root is backed by one or more Proof Requirements that declare how and at which workflow stage satisfaction is established.
_Avoid_: unverifiable instructions, prompt guidance, or agent memory presented as authoritative process

**Proof Requirement**:
A configured declaration of evidence expected at a workflow stage, whether it is required or advisory, and how missing or failed proof is handled.
It connects a Root to a Gate, human approval, external attestation, or other proof without itself producing Evidence.
_Avoid_: Gate, Policy, arbitrary stopping point, proof result, duplicating the Workflow's action sequence

**Workflow**:
The single coordinating process instance that governs a Plot's ordered or conditional Seams and lifecycle transitions.
It is selected at Establishment from an authoritative source, normally the Repository that establishes the Plot, while additional Repositories contribute scoped requirements and enforcement without contributing competing lifecycles.
_Avoid_: Progress, skill sequence, multiple competing Workflows inside one Plot, automatic Workflow merging

**Workflow Seam**:
A named workflow boundary where applicable Proof Requirements must be resolved before a consequential action or authoritative transition may cross it.
A Beislið skill may request its evaluation, Nopal Core alone accepts it, and a Host provides the actual Enforcement Coverage.
_Avoid_: Progress phase, skill completion, conversational inference, treating a request as acceptance

**Policy**:
A configured authorization rule that determines whether a requested action may run automatically, requires approval, or is denied, and what execution isolation it requires.
It evaluates action identity, action class, operating mode, and Authority; it neither establishes whether a Root is true nor decides what a failing Gate means for Progress.
_Avoid_: Root, Gate, lifecycle rule, Guidance, unenforced preference, treating authorization as proof

**Gate**:
A configured deterministic check that evaluates a specific claim through a declared verification method and produces an inspectable result with Evidence.
It establishes what was observed; its associated Proof Requirement determines at which workflow stage the result is expected and whether failure blocks.
_Avoid_: Policy, Spine, Root, advisory check presented as protection, treating a result as a decision

**Guidance**:
Non-authoritative advice or context that may shape how agents work but cannot establish, weaken, or satisfy Roots or Spines.
_Avoid_: presenting Guidance as a requirement or as Evidence of process satisfaction

**Spines**:
An effective, inspectable enforcement chain composed from the Roots, Proof Requirements, Policies, Gate results, authority rules, lifecycle rules, and isolation controls applicable at a workflow seam.
It is derived rather than separately authored, and is a Spine only where enforcement is deterministic and non-bypassable.
_Avoid_: thorn, opaque aggregate status, separately configured duplicate rule, guardrails that exist only as prose inside an agent prompt

**Exception**:
An explicit, authorized, and inspectable decision to weaken an otherwise applicable Root for a defined Plot or Subplot.
It preserves the original requirement and records scope, rationale, authority, and validity; agents may request an Exception but approval comes only from a configured human authority or deterministic rule.
_Avoid_: silent overrides, undocumented bypasses, agent-approved exemptions, or model judgment presented as authority

**Fruit**:
The reviewable result or deliverable produced by a Plot.
Fruit can be accepted or rejected, and Evidence establishes whether it satisfies the Plot's intent and Roots.
_Avoid_: using Fruit as a synonym for Evidence or treating its existence as proof of success

**Provenance**:
The inspectable record of who or what authored, approved, observed, attested, or derived an item, together with its source scope and effective time.
Every authoritative decision and assurance fact retains Provenance through aggregation and presentation.
_Avoid_: user-defined as a complete explanation, anonymous system state, losing component sources inside a derived result

**Evidence**:
The inspectable facts and artifacts that establish what happened in a Plot and whether its intended outcome and Roots were satisfied.
_Avoid_: agent self-reports, confidence, or successful exit status as substitutes for evidence

**External Observation**:
An inspectable fact about Repository or workflow state that Nopal detects but no managed Host mediated.
It may contribute Evidence of what exists but never retroactively establishes Enforcement Coverage or proves that an applicable Spine was followed.
_Avoid_: Nopal action, inferred compliance, retroactive assurance

**Enforcement Coverage**:
The inspectable set of guarantees actually provided by the Hosts involved in a Plot, evaluated against its Proof Requirements.
It records concrete mediation, binding, supervision, authority, and Evidence protections rather than assigning trust by product or collapsing coverage into a score.
_Avoid_: properly equipped, trusted host tier, product reputation, assurance score

**Assurance**:
Evidence-backed confidence that managed work followed its configured process and produced the claimed outcome under the Enforcement Coverage actually available.
It comes from observed facts, Gates, recorded decisions, and inspectable artifacts - never from trusting an agent's self-report or Host identity alone.
_Avoid_: an opaque status or score, or treating agent confidence, completion claims, or a successful exit as proof

**Beislið**:
The skill-shaped surface over Nopal Core for generic hosts such as Claude Code; it renders and routes, never decides.
It works without the Nopal application or Pi distribution but intentionally depends on the shared Nopal Core engine once parity is reached.
_Avoid_: treating Beislið as an independent harness with its own deterministic core, or treating its dependency on Nopal Core as a dependency on the Nopal application

**Rondo**:
The AFK execution engine; a separate product integrated over the execution contract.

**Memento**:
The curated memory vault; a separate product integrated over the memory contract.

**Pi**:
The kernel and host for agent loops, sessions, tool execution, and foundational UI surfaces.
Nopal wraps Pi and preserves familiar Pi workflows; it never reimplements Pi or presents Pi as the outward product identity.

**Contract**:
A versioned boundary with a foreign product; exactly two exist - execution (Rondo) and memory (Memento).
_Avoid_: calling Nopal Core's own envelopes, config schemas, or process artifacts "contracts" (they are its versioned product surface: safety lattices frozen, vocabularies open)

## Relationships

- **Nopal Core** is the single deterministic engine; every surface fetches decisions from it and carries no semantics of its own
- The **Nopal** Pi distribution and **Beislið** skills are sibling surfaces over **Nopal Core**, while the engine remains product-owned by Nopal
- **Beislið** works independently of the Nopal application and Pi distribution but depends on **Nopal Core** once its parallel Python deterministic core retires at parity
- **Workflow** determines the active **Workflow Seam**, applicable Proof Requirements and Gates determine expected proof, and **Policy** authorizes the requested action; **Nopal Core** composes them into the effective **Spine** without a separately authored proof-to-action mapping
- A **Beislið** skill may request evaluation of the active **Workflow Seam**; in the Nopal Pi distribution, the extension executes Core's returned plan and enforces its decision before allowing the workflow to cross the seam
- Standalone **Beislið** may use the same seam protocol but never claims Nopal-equivalent enforcement unless its Host demonstrates equivalent **Enforcement Coverage**
- Sharing **Nopal Core** provides decision parity between Hosts but provides Assurance parity only where their proven **Enforcement Coverage** satisfies the same Proof Requirements
- Several Hosts may compose their **Enforcement Coverage** for one Plot, but coverage at one workflow seam never implies observation or control at another
- **Rondo** (execution) and **Memento** (memory) are separate products, integrated over the two contracts
- A first conversation may create an unassigned **Provisional Plot** without requiring prior Repository or Workspace selection
- A Provisional Plot follows only Nopal's fixed minimal Workflow; Establishment replaces it with the selected authoritative Workflow while preserving Plot identity and history
- An established **Plot** may involve one or more **Repositories** and use several **Workspaces** during its life
- Every Plot owns exactly one coordinating **Workflow** instance, even when it spans several Repositories
- Establishment selects the Plot's authoritative Workflow, normally from the Repository that establishes it; Nopal Core never merges several Repository lifecycle graphs automatically
- Additional Repositories contribute scoped Roots, Proof Requirements, Gates, and Policies at the selected Workflow's Seams without replacing its lifecycle
- Work that must follow a genuinely different Workflow becomes a sibling Subplot or independent Plot, while the parent Plot owns cross-Repository integration proof and final closure
- Each **Repository** supplies its own applicable Roots and Spines; a **Workspace** never overrides them
- A **Session** is bound to one **Workspace** for its lifetime; changing Workspace starts another Session under the same Plot
- Workspace mutation defaults to one writer; the applicable **Spine** deterministically enforces the configured concurrency decision
- Removing a **Workspace** never removes its Plot, Sessions, or Evidence
- **Establishment** preserves a Provisional Plot's identity, Seed, and conversation history rather than replacing it
- Repository configuration changes never silently alter an established Plot's effective Workflow, Roots, Proof Requirements, or Gate declarations; adopting a new configuration requires an explicit authorized decision with inspectable Provenance
- A newly restrictive **Policy** applies immediately to active Plots, while a policy weakening takes effect only through explicit authorized adoption
- An earlier configuration snapshot can preserve a stricter Policy but can never preserve access that a newer applicable Policy denies
- A **Subplot** inherits every applicable **Root** from its parent, may add stricter Roots, and may weaken an inherited Root only through an approved **Exception**
- A **Subplot** has exactly one parent so inheritance, Authority, requiredness, and lineage remain unambiguous
- Subplots are one level deep; a Subplot may use Subagents but can never parent another Subplot
- Work beneath a Subplot that needs another independent assurance boundary becomes an independent Plot whose accepted Fruit and Evidence are referenced explicitly
- Work whose accepted **Fruit** is reusable by several Plots is an independent Plot, not a multi-parent Subplot; each consumer evaluates that Fruit and its Evidence at its own Workflow Seams
- A consumer pins the specific accepted Fruit and Evidence snapshot it relies on; later changes, Offshoots, or replacement Fruit never rewrite that historical dependency
- Accepted Subplot **Fruit** contributes to its parent but never advances or closes the parent automatically
- A Subplot is never intrinsically required or optional; the parent's **Proof Requirements** declare whether and at which Workflow Seam its accepted Fruit is required
- A parent Plot closes only when its own Intent, integration Roots, Proof Requirements, and effective **Spine** accept closure
- Missing or rejected Subplot Fruit follows the parent's applicable Proof Requirement and failure policy; absent Fruit does not block when no parent requirement makes it necessary
- A parent Plot cannot close while an attached Subplot remains Active, and closing the parent never closes a Subplot implicitly
- Before parent closure, an unfinished Subplot whose Fruit is not required must either close with absent Fruit or become an independent Plot while preserving its Provenance and lineage
- Changing **Execution Mode** never creates a Plot or Subplot; Unattended work that needs interaction opens an Interactive Session within the same assurance boundary when Intent and Fruit remain unchanged
- If an Unattended execution discovers a separately accepted outcome, it may propose a sibling Subplot under the root Plot or an independent Plot, but never a nested Subplot
- Delegating execution creates a **Subagent**, while decomposing an intended outcome creates a **Subplot**
- A Plot or Subplot may use several Subagents, and a Subplot may be completed without any Subagent
- A Subagent remains in its current Plot scope unless an authorized Subplot is established first; it may propose but never silently create a new assurance boundary
- An instruction without an associated **Proof Requirement** is **Guidance**, not a **Root**
- A **Root** states what must be true, a **Policy** authorizes an action and its isolation, a **Gate** evaluates, a **Spine** enforces lifecycle consequences and other boundaries, and **Evidence** records what happened
- A **Proof Requirement** makes proof required or advisory at a named workflow stage and declares whether missing or failed proof blocks, warns, or requires human intervention
- A Gate failure becomes a stopping point only when an applicable required **Proof Requirement** says it blocks at the current workflow stage
- Gate Evidence records its revision and Workspace; functional changes require fresh applicable Gate results at the next workflow seam rather than a general dependency-level invalidation graph
- Scoped configuration composes per concept: Roots accumulate, Policies use declared precedence, Gate definitions compose while their results are produced fresh, Authority may narrow, and Guidance may override; a narrower scope cannot silently weaken broader assurance
- Users author a Spine's components; Nopal derives the effective **Spine** and preserves each component's source, result, and enforcement outcome for inspection
- Authorship can propose content, but only configured **Authority** makes it binding; **Provenance** preserves that distinction
- Repository actions performed outside Nopal are reconciled as **External Observations** with explicit Provenance and never receive retroactive Enforcement Coverage
- Nopal exposes **Progress**, **Fruit**, Root satisfaction, **Evidence**, and **Exceptions** independently and never collapses them into an opaque assurance score
- **Ready** is derived from a Planned Plot's Roots, Spines, and Conditions; it is not a Progress phase
- Blocked, Waiting, and Paused are **Conditions**; failed, interrupted, completed, and terminated are execution outcomes; accepted, rejected, and absent describe **Fruit**
- Closed is terminal for a **Plot**; later work begins in a new linked Plot so accepted Fruit, Root results, and Evidence remain historically stable
- Closed Plots may leave the default **Field** view, but archival is presentation behavior rather than a Progress phase or historical mutation
- Only a **Provisional Plot** that was never established may expire automatically
- An **Offshoot** resolves the Roots applicable when it is created and never recalculates or replaces its predecessor's historical Root snapshot

## Example dialogue

> **Dev:** "Where do I change which gates run pre-PR - the Beislið skill or the workflow prose?"
> **Domain expert:** "Neither decides anything. Gate selection lives in repository config and **Nopal Core** answers it; the Beislið skill and the Nopal Pi extension both ask the same core and render or execute its result."

## Resolved identity decisions

- **Nopal** is the final product name, with `nopal.sh` as its canonical public address.
- **Nopal Core** is the shared deterministic engine and is intentionally usable by Beislið without the Nopal application or Pi distribution.
- The technical cutover is complete: `nopal`, `.nopal/`, and `nopal.*/v1` are canonical, with no compatibility aliases or dual discovery.
- Olin was an earlier name for the deterministic core and is retired.
- Teotl and Vistro were earlier names for the whole product concept and are retired.
- The coordinator lives in `crates/nopal-cli`, while the deterministic engine lives in `crates/nopal-core`.
