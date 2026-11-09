---
trigger: always_on
description: System Architecture & Engineering Invariants (GovTech Core)
---

# System Architecture & Engineering Invariants (GovTech Core)

## 1. Zero-Float Financial Policy
- NEVER use `f32` or `f64` anywhere in the codebase. Clippy `float_arithmetic` is set to `deny`.
- All monetary values MUST use the `Cents(i64)` newtype or `rust_decimal::Decimal` with explicit rounding strategies (`MidpointNearestEven` or fiscal truncation rules defined in `gov-domain`).

## 2. Error Handling & Panic Safety
- `unwrap()`, `expect()`, and `panic!()` are strictly forbidden in library and binary crates (enforced via Clippy).
- Every fallible operation MUST return a typed `Result<T, DomainError>` using `thiserror`.
- `unsafe` blocks are forbidden (`unsafe_code = "forbid"`).

## 3. Database & Ledger Immutability (CONAC Compliance)
- The `ledger_entries` and `payment_events` tables are APPEND-ONLY.
- NEVER generate SQL `UPDATE` or `DELETE` statements against financial ledger tables. Corrections or cancellations MUST be recorded as compensating contra-entries (`Reversal` entry linked to original `entry_id`).
- Every state-mutating endpoint or webhook handler MUST require an `IdempotencyKey` and execute inside an ACID database transaction (`sqlx::Transaction<'_, Postgres>`).

## 4. Strict Layer Decoupling
- `gov-domain` MUST NOT depend on `sqlx`, `axum`, `tonic`, or any I/O crate. It contains pure domain logic, newtypes, and trait definitions.
- `gov-core-api` NEVER connects directly to a municipal database. All communication with municipal legacy systems happens asynchronously through `gov-proto` (gRPC stream) and the `sync_outbox` table.
- All UUIDs for database primary keys and ledger entries MUST use `Uuid::now_v7()` (time-ordered UUIDv7) to prevent B-Tree index fragmentation in PostgreSQL.

