//! Physics — see SPEC.md §10.
//!
//! TODO:
//!   - Layer 1: query primitives (§10.1) — reuse SVO traversal from renderer
//!   - Layer 2: rigid bodies (§10.2) — AABB/sphere, kinds (Static/Dynamic/Kinematic),
//!     collision filtering, fixed-step integration
//!   - Layer 3: cellular automata (§10.3) — sparse active set, deterministic drain order,
//!     per-port budget caps
//!   - Determinism (§10.5) — replay action stream rebuilt from cart input
