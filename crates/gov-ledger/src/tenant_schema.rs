use gov_domain::{DomainError, TenantId};
use sqlx::{AssertSqlSafe, Executor, PgConnection, PgPool};

use crate::error::LedgerError;

const INIT_MIGRATION_SQL: &str = include_str!("../../../migrations/0001_init_ledger_and_outbox.sql");

/// Construye de forma segura la sentencia DDL `CREATE SCHEMA IF NOT EXISTS` para un [`TenantId`] validado.
///
/// Aunque [`TenantId`] ya garantiza en construcción que solo contiene `[a-z0-9_]` (`3..=32` caracteres),
/// esta función aplica citación estricta de identificador SQL como defensa en profundidad.
#[must_use]
pub fn sanitize_and_build_create_schema_sql(tenant_id: &TenantId) -> String {
    let schema = tenant_id.as_str();
    format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\";")
}

/// Construye la sentencia `SET LOCAL search_path` para aislar una transacción en el esquema del municipio.
#[must_use]
pub fn build_set_search_path_sql(tenant_id: &TenantId) -> String {
    let schema = tenant_id.as_str();
    format!("SET LOCAL search_path TO \"{schema}\", public;")
}

/// Verifica en defensa en profundidad que el [`TenantId`] cumpla el invariante léxico antes de interpolar DDL.
///
/// # Errors
/// Devuelve [`LedgerError::Domain`] si el identificador viola las reglas de esquema.
pub fn validate_tenant_schema_identifier(tenant_id: &TenantId) -> Result<(), LedgerError> {
    let revalidated = TenantId::try_new(tenant_id.as_str())?;
    if revalidated != *tenant_id {
        return Err(LedgerError::Domain(DomainError::InvalidTenantId(
            tenant_id.to_string(),
        )));
    }
    Ok(())
}

/// Configura `SET LOCAL search_path` dentro de una transacción abierta tras auditar el [`TenantId`].
///
/// # Errors
/// Devuelve [`LedgerError`] si el [`TenantId`] es inválido o falla la sentencia en PostgreSQL.
pub async fn set_local_tenant_search_path(
    conn: &mut PgConnection,
    tenant_id: &TenantId,
) -> Result<(), LedgerError> {
    validate_tenant_schema_identifier(tenant_id)?;
    let search_path_sql = build_set_search_path_sql(tenant_id);
    sqlx::query(AssertSqlSafe(search_path_sql))
        .execute(conn)
        .await?;
    Ok(())
}

/// Crea el esquema aislado del municipio (`Schema-per-Tenant`) e inicializa las tablas contables
/// y los triggers CONAC de inmutabilidad (`append-only`) dentro de una transacción ACID.
///
/// # Errors
/// Devuelve [`LedgerError`] si falla la validación del identificador o la ejecución en PostgreSQL.
pub async fn ensure_tenant_schema(pool: &PgPool, tenant_id: &TenantId) -> Result<(), LedgerError> {
    validate_tenant_schema_identifier(tenant_id)?;

    let mut tx = pool.begin().await?;
    let create_schema_sql = sanitize_and_build_create_schema_sql(tenant_id);

    sqlx::query(AssertSqlSafe(create_schema_sql))
        .execute(&mut *tx)
        .await?;
    set_local_tenant_search_path(&mut tx, tenant_id).await?;
    (&mut *tx).execute(INIT_MIGRATION_SQL).await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_and_build_create_schema_sql_for_valid_tenant() -> Result<(), DomainError> {
        let tenant = TenantId::try_new("mun_orizaba_2026")?;
        let ddl = sanitize_and_build_create_schema_sql(&tenant);
        assert_eq!(ddl, "CREATE SCHEMA IF NOT EXISTS \"mun_orizaba_2026\";");

        let search_path = build_set_search_path_sql(&tenant);
        assert_eq!(
            search_path,
            "SET LOCAL search_path TO \"mun_orizaba_2026\", public;"
        );
        Ok(())
    }

    #[test]
    fn test_validate_tenant_schema_identifier_passes_valid_tenant() -> Result<(), LedgerError> {
        let tenant = TenantId::try_new("mun_cordoba")?;
        validate_tenant_schema_identifier(&tenant)?;
        Ok(())
    }
}
