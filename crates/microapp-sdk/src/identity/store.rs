use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;

use super::error::IdentityError;
use nexo_tool_meta::marketing::{
    Company, CompanyId, EnrichmentStatus, Person, PersonId, TenantIdRef,
};

/// PersonEmail row — one address mapped to its owning person.
/// Multi-email merges append rows here; the primary email lives
/// on the `Person` record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonEmail {
    pub person_id: PersonId,
    pub tenant_id: TenantIdRef,
    pub email: String,
    pub verified: bool,
    pub added_at_ms: i64,
}

/// LID ↔ PN mapping row. WhatsApp protocol
/// announces a migration whenever a contact's phone-number
/// JID (`573001234567@s.whatsapp.net`) gets a corresponding
/// LID-namespace JID (`123456789@lid`). Both Baileys + whatsmeow
/// track this as a first-class concept; the marketing
/// extension's duplicate matcher consults the mapping to
/// avoid creating a second `Person` for the same human.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LidPnMapping {
    /// Tenant scope.
    pub tenant_id: TenantIdRef,
    /// Canonical LID user portion (no device, no agent).
    pub lid_user: String,
    /// Canonical PN user portion (no device, no agent).
    pub pn_user: String,
    /// First time the mapping was observed. Subsequent
    /// upserts of the same pair don't bump this — keep the
    /// audit-flavoured \"first seen\" stamp.
    pub observed_at_ms: i64,
}

/// PersonPhone row — one phone identifier mapped to its
/// owning person. Mirrors `PersonEmail` for the WhatsApp /
/// SMS side: a contact's E.164 phone (or platform-specific
/// JID like `573001234567@s.whatsapp.net`) resolves to the
/// same `Person` already known to the email pipeline,
/// powering the cross-channel duplicate detector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonPhone {
    pub person_id: PersonId,
    pub tenant_id: TenantIdRef,
    /// Canonical phone identifier. The caller normalises:
    /// E.164 (`+573001234567`) preferred; platform JID
    /// (`573001234567@s.whatsapp.net`) accepted when the
    /// caller can't strip the suffix. Match is exact —
    /// caller is responsible for normalising on upsert AND
    /// lookup.
    pub phone: String,
    pub verified: bool,
    pub added_at_ms: i64,
}

// ── Trait surface ───────────────────────────────────────────────

#[async_trait]
pub trait PersonStore: Send + Sync {
    async fn upsert(&self, tenant_id: &str, person: &Person) -> Result<Person, IdentityError>;
    async fn get(&self, tenant_id: &str, id: &PersonId) -> Result<Option<Person>, IdentityError>;
    async fn find_by_email(
        &self,
        tenant_id: &str,
        email: &str,
    ) -> Result<Option<Person>, IdentityError>;
    /// Every person on the tenant whose `company_id` matches
    /// `company_id`. Powers the duplicate-person
    /// matcher's name+company fuzzy signal so the search
    /// space isn't artificially narrowed to email-matched
    /// candidates only. Caller-supplied `limit` clamps the
    /// result row count; default impl returns no more than
    /// 100 rows ordered by `last_seen_at_ms` desc.
    ///
    /// Default impl returns an empty `Vec` so existing
    /// in-tree implementations (test fakes) don't have to
    /// implement the method until they want the fuzzy signal.
    async fn list_by_company(
        &self,
        _tenant_id: &str,
        _company_id: &str,
        _limit: usize,
    ) -> Result<Vec<Person>, IdentityError> {
        Ok(Vec::new())
    }
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError>;
}

#[async_trait]
pub trait PersonEmailStore: Send + Sync {
    async fn add(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
        email: &str,
        verified: bool,
    ) -> Result<PersonEmail, IdentityError>;
    async fn list_for_person(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
    ) -> Result<Vec<PersonEmail>, IdentityError>;
    async fn find_owner(
        &self,
        tenant_id: &str,
        email: &str,
    ) -> Result<Option<PersonId>, IdentityError>;
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError>;
}

#[async_trait]
pub trait PersonPhoneStore: Send + Sync {
    /// Upsert one `(person_id, phone)` link. Idempotent on
    /// `(tenant_id, phone)` so re-adding the same phone
    /// updates `verified` + `added_at_ms` instead of
    /// erroring.
    async fn add(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
        phone: &str,
        verified: bool,
    ) -> Result<PersonPhone, IdentityError>;
    /// All phones owned by one person. Useful for the
    /// duplicate-detection candidate scan.
    async fn list_for_person(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
    ) -> Result<Vec<PersonPhone>, IdentityError>;
    /// Find the person owning a given phone identifier.
    /// `None` ⇒ unseen. Cross-tenant misses always return
    /// `None`.
    async fn find_owner(
        &self,
        tenant_id: &str,
        phone: &str,
    ) -> Result<Option<PersonId>, IdentityError>;
    /// Cascade-delete every link for one tenant. Returns
    /// the number of rows removed.
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError>;
}

#[async_trait]
pub trait LidPnMappingStore: Send + Sync {
    /// Persist one LID ↔ PN pair. Idempotent on the LID
    /// side: re-upserting the same `(tenant, lid_user)`
    /// updates the PN component + leaves `observed_at_ms`
    /// unchanged (first-seen semantics). Caller normalises
    /// both JIDs via [`super::parse_jid`] before calling —
    /// the trait stores the user portion only, so device +
    /// agent suffixes never enter the index.
    async fn put(
        &self,
        tenant_id: &str,
        lid_user: &str,
        pn_user: &str,
    ) -> Result<LidPnMapping, IdentityError>;

    /// Look up the PN user portion that the protocol paired
    /// with this LID. `None` ⇒ no mapping recorded yet
    /// (caller treats the LID as its own identity).
    async fn get_pn_for_lid(
        &self,
        tenant_id: &str,
        lid_user: &str,
    ) -> Result<Option<String>, IdentityError>;

    /// Reverse direction — find the LID paired with a PN.
    async fn get_lid_for_pn(
        &self,
        tenant_id: &str,
        pn_user: &str,
    ) -> Result<Option<String>, IdentityError>;

    /// Cascade-delete every row for one tenant. Returns the
    /// number of rows removed.
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError>;
}

#[async_trait]
pub trait CompanyStore: Send + Sync {
    async fn upsert(&self, tenant_id: &str, company: &Company) -> Result<Company, IdentityError>;
    async fn get(&self, tenant_id: &str, id: &CompanyId) -> Result<Option<Company>, IdentityError>;
    async fn find_by_domain(
        &self,
        tenant_id: &str,
        domain: &str,
    ) -> Result<Option<Company>, IdentityError>;
    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError>;
}

// ── Sqlite implementations ──────────────────────────────────────

const MIGRATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS persons (
    id                     TEXT NOT NULL,
    tenant_id              TEXT NOT NULL,
    primary_name           TEXT NOT NULL,
    primary_email          TEXT NOT NULL,
    company_id             TEXT,
    enrichment_status      TEXT NOT NULL,
    enrichment_confidence  REAL NOT NULL,
    tags_json              TEXT NOT NULL DEFAULT '[]',
    created_at_ms          INTEGER NOT NULL,
    last_seen_at_ms        INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_persons_tenant_email
    ON persons(tenant_id, primary_email);

CREATE TABLE IF NOT EXISTS person_emails (
    tenant_id    TEXT NOT NULL,
    person_id    TEXT NOT NULL,
    email        TEXT NOT NULL,
    verified     INTEGER NOT NULL DEFAULT 0,
    added_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, email)
);
CREATE INDEX IF NOT EXISTS idx_person_emails_tenant_person
    ON person_emails(tenant_id, person_id);

CREATE TABLE IF NOT EXISTS person_phones (
    tenant_id    TEXT NOT NULL,
    person_id    TEXT NOT NULL,
    phone        TEXT NOT NULL,
    verified     INTEGER NOT NULL DEFAULT 0,
    added_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, phone)
);
CREATE INDEX IF NOT EXISTS idx_person_phones_tenant_person
    ON person_phones(tenant_id, person_id);

CREATE TABLE IF NOT EXISTS lid_pn_mappings (
    tenant_id      TEXT NOT NULL,
    lid_user       TEXT NOT NULL,
    pn_user        TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, lid_user)
);
CREATE INDEX IF NOT EXISTS idx_lid_pn_mappings_pn
    ON lid_pn_mappings(tenant_id, pn_user);

CREATE TABLE IF NOT EXISTS companies (
    id                  TEXT NOT NULL,
    tenant_id           TEXT NOT NULL,
    domain              TEXT NOT NULL,
    name                TEXT NOT NULL,
    industry            TEXT,
    size_band           TEXT,
    enriched_at_ms      INTEGER,
    is_personal_domain  INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_companies_tenant_domain
    ON companies(tenant_id, domain);
"#;

/// Open a sqlite pool against `path`, run migrations, return.
/// Path can be `:memory:` for tests.
pub async fn open_pool(path: impl AsRef<Path>) -> Result<SqlitePool, IdentityError> {
    let p = path.as_ref().to_string_lossy().to_string();
    let conn_str = if p == ":memory:" {
        "sqlite::memory:".to_string()
    } else {
        if let Some(parent) = path.as_ref().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        format!("sqlite://{p}")
    };
    let opts = SqliteConnectOptions::from_str(&conn_str)
        .map_err(|e| IdentityError::Migration(e.to_string()))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await?;
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .ok();
    sqlx::query(MIGRATION_SQL).execute(&pool).await?;
    Ok(pool)
}

/// SQLite-backed person store. Cheap to clone (`Arc`-shared
/// pool); pass into `tokio::spawn` etc. without copying.
#[derive(Clone)]
pub struct SqlitePersonStore {
    pool: SqlitePool,
}

impl SqlitePersonStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl PersonStore for SqlitePersonStore {
    async fn upsert(&self, tenant_id: &str, person: &Person) -> Result<Person, IdentityError> {
        validate_email(&person.primary_email)?;
        let tags_json = serde_json::to_string(&person.tags).unwrap_or_else(|_| "[]".into());
        let company_id_str = person.company_id.as_ref().map(|c| c.0.clone());
        let status_str =
            serde_json::to_string(&person.enrichment_status).unwrap_or_else(|_| "\"none\"".into());
        // Strip serde quotes — store the bare snake_case value.
        let status_bare = status_str.trim_matches('"').to_string();
        sqlx::query(
            "INSERT INTO persons \
             (id, tenant_id, primary_name, primary_email, company_id, \
              enrichment_status, enrichment_confidence, tags_json, \
              created_at_ms, last_seen_at_ms) \
             VALUES (?,?,?,?,?,?,?,?,?,?) \
             ON CONFLICT(tenant_id, id) DO UPDATE SET \
              primary_name=excluded.primary_name, \
              primary_email=excluded.primary_email, \
              company_id=excluded.company_id, \
              enrichment_status=excluded.enrichment_status, \
              enrichment_confidence=excluded.enrichment_confidence, \
              tags_json=excluded.tags_json, \
              last_seen_at_ms=excluded.last_seen_at_ms",
        )
        .bind(&person.id.0)
        .bind(tenant_id)
        .bind(&person.primary_name)
        .bind(&person.primary_email)
        .bind(company_id_str)
        .bind(&status_bare)
        .bind(person.enrichment_confidence as f64)
        .bind(&tags_json)
        .bind(person.created_at_ms)
        .bind(person.last_seen_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(person.clone())
    }

    async fn get(&self, tenant_id: &str, id: &PersonId) -> Result<Option<Person>, IdentityError> {
        let row = sqlx::query_as::<_, PersonRow>(
            "SELECT id, tenant_id, primary_name, primary_email, company_id, \
                    enrichment_status, enrichment_confidence, tags_json, \
                    created_at_ms, last_seen_at_ms \
             FROM persons WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(&id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(PersonRow::into_person))
    }

    async fn find_by_email(
        &self,
        tenant_id: &str,
        email: &str,
    ) -> Result<Option<Person>, IdentityError> {
        validate_email(email)?;
        let row = sqlx::query_as::<_, PersonRow>(
            "SELECT id, tenant_id, primary_name, primary_email, company_id, \
                    enrichment_status, enrichment_confidence, tags_json, \
                    created_at_ms, last_seen_at_ms \
             FROM persons WHERE tenant_id = ? AND primary_email = ?",
        )
        .bind(tenant_id)
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(PersonRow::into_person))
    }

    async fn list_by_company(
        &self,
        tenant_id: &str,
        company_id: &str,
        limit: usize,
    ) -> Result<Vec<Person>, IdentityError> {
        // Hard cap so a misconfigured caller can't trigger an
        // unbounded scan of the tenant's full person table.
        let bounded = limit.clamp(1, 1000) as i64;
        let rows = sqlx::query_as::<_, PersonRow>(
            "SELECT id, tenant_id, primary_name, primary_email, company_id, \
                    enrichment_status, enrichment_confidence, tags_json, \
                    created_at_ms, last_seen_at_ms \
             FROM persons \
             WHERE tenant_id = ? AND company_id = ? \
             ORDER BY last_seen_at_ms DESC \
             LIMIT ?",
        )
        .bind(tenant_id)
        .bind(company_id)
        .bind(bounded)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(PersonRow::into_person).collect())
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError> {
        let r = sqlx::query("DELETE FROM persons WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PersonRow {
    id: String,
    tenant_id: String,
    primary_name: String,
    primary_email: String,
    company_id: Option<String>,
    enrichment_status: String,
    enrichment_confidence: f64,
    tags_json: String,
    created_at_ms: i64,
    last_seen_at_ms: i64,
}

impl PersonRow {
    fn into_person(self) -> Person {
        let tags: Vec<String> = serde_json::from_str(&self.tags_json).unwrap_or_default();
        let status: EnrichmentStatus =
            serde_json::from_str(&format!("\"{}\"", self.enrichment_status))
                .unwrap_or(EnrichmentStatus::None);
        Person {
            id: PersonId(self.id),
            tenant_id: TenantIdRef(self.tenant_id),
            primary_name: self.primary_name,
            primary_email: self.primary_email,
            alt_emails: Vec::new(),
            company_id: self.company_id.map(CompanyId),
            enrichment_status: status,
            enrichment_confidence: self.enrichment_confidence as f32,
            tags,
            created_at_ms: self.created_at_ms,
            last_seen_at_ms: self.last_seen_at_ms,
        }
    }
}

/// SQLite-backed person_email store.
#[derive(Clone)]
pub struct SqlitePersonEmailStore {
    pool: SqlitePool,
}

impl SqlitePersonEmailStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl PersonEmailStore for SqlitePersonEmailStore {
    async fn add(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
        email: &str,
        verified: bool,
    ) -> Result<PersonEmail, IdentityError> {
        validate_email(email)?;
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO person_emails (tenant_id, person_id, email, verified, added_at_ms) \
             VALUES (?,?,?,?,?) \
             ON CONFLICT(tenant_id, email) DO UPDATE SET \
              person_id=excluded.person_id, \
              verified=excluded.verified",
        )
        .bind(tenant_id)
        .bind(&person_id.0)
        .bind(email)
        .bind(verified as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(PersonEmail {
            person_id: person_id.clone(),
            tenant_id: TenantIdRef(tenant_id.to_string()),
            email: email.to_string(),
            verified,
            added_at_ms: now,
        })
    }

    async fn list_for_person(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
    ) -> Result<Vec<PersonEmail>, IdentityError> {
        let rows = sqlx::query_as::<_, PersonEmailRow>(
            "SELECT person_id, tenant_id, email, verified, added_at_ms \
             FROM person_emails WHERE tenant_id = ? AND person_id = ? \
             ORDER BY added_at_ms ASC",
        )
        .bind(tenant_id)
        .bind(&person_id.0)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(PersonEmailRow::into_email).collect())
    }

    async fn find_owner(
        &self,
        tenant_id: &str,
        email: &str,
    ) -> Result<Option<PersonId>, IdentityError> {
        validate_email(email)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT person_id FROM person_emails WHERE tenant_id = ? AND email = ?")
                .bind(tenant_id)
                .bind(email)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(id,)| PersonId(id)))
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError> {
        let r = sqlx::query("DELETE FROM person_emails WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PersonEmailRow {
    person_id: String,
    tenant_id: String,
    email: String,
    verified: i64,
    added_at_ms: i64,
}

impl PersonEmailRow {
    fn into_email(self) -> PersonEmail {
        PersonEmail {
            person_id: PersonId(self.person_id),
            tenant_id: TenantIdRef(self.tenant_id),
            email: self.email,
            verified: self.verified != 0,
            added_at_ms: self.added_at_ms,
        }
    }
}

/// SQLite-backed person_phone store. Mirror of
/// [`SqlitePersonEmailStore`] for the WhatsApp / SMS side.
#[derive(Clone)]
pub struct SqlitePersonPhoneStore {
    pool: SqlitePool,
}

impl SqlitePersonPhoneStore {
    /// Wrap an open pool. Run [`open_pool`] first.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    /// Borrow the underlying pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl PersonPhoneStore for SqlitePersonPhoneStore {
    async fn add(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
        phone: &str,
        verified: bool,
    ) -> Result<PersonPhone, IdentityError> {
        if phone.trim().is_empty() {
            return Err(IdentityError::InvalidEmail("phone empty".into()));
        }
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO person_phones (tenant_id, person_id, phone, verified, added_at_ms) \
             VALUES (?,?,?,?,?) \
             ON CONFLICT(tenant_id, phone) DO UPDATE SET \
              person_id=excluded.person_id, \
              verified=excluded.verified",
        )
        .bind(tenant_id)
        .bind(&person_id.0)
        .bind(phone)
        .bind(verified as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(PersonPhone {
            person_id: person_id.clone(),
            tenant_id: TenantIdRef(tenant_id.to_string()),
            phone: phone.to_string(),
            verified,
            added_at_ms: now,
        })
    }

    async fn list_for_person(
        &self,
        tenant_id: &str,
        person_id: &PersonId,
    ) -> Result<Vec<PersonPhone>, IdentityError> {
        let rows = sqlx::query_as::<_, PersonPhoneRow>(
            "SELECT person_id, tenant_id, phone, verified, added_at_ms \
             FROM person_phones WHERE tenant_id = ? AND person_id = ? \
             ORDER BY added_at_ms ASC",
        )
        .bind(tenant_id)
        .bind(&person_id.0)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(PersonPhoneRow::into_phone).collect())
    }

    async fn find_owner(
        &self,
        tenant_id: &str,
        phone: &str,
    ) -> Result<Option<PersonId>, IdentityError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT person_id FROM person_phones WHERE tenant_id = ? AND phone = ?")
                .bind(tenant_id)
                .bind(phone)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(id,)| PersonId(id)))
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError> {
        let r = sqlx::query("DELETE FROM person_phones WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PersonPhoneRow {
    person_id: String,
    tenant_id: String,
    phone: String,
    verified: i64,
    added_at_ms: i64,
}

impl PersonPhoneRow {
    fn into_phone(self) -> PersonPhone {
        PersonPhone {
            person_id: PersonId(self.person_id),
            tenant_id: TenantIdRef(self.tenant_id),
            phone: self.phone,
            verified: self.verified != 0,
            added_at_ms: self.added_at_ms,
        }
    }
}

/// SQLite-backed [`LidPnMappingStore`].
/// Tenant-keyed; PK on `(tenant_id, lid_user)` with a
/// covering index on `(tenant_id, pn_user)` so reverse
/// lookups don't trigger a table scan.
#[derive(Clone)]
pub struct SqliteLidPnMappingStore {
    pool: SqlitePool,
}

impl SqliteLidPnMappingStore {
    /// Wrap an open pool. Run [`open_pool`] first.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    /// Borrow the underlying pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl LidPnMappingStore for SqliteLidPnMappingStore {
    async fn put(
        &self,
        tenant_id: &str,
        lid_user: &str,
        pn_user: &str,
    ) -> Result<LidPnMapping, IdentityError> {
        if lid_user.trim().is_empty() || pn_user.trim().is_empty() {
            return Err(IdentityError::InvalidEmail(
                "lid_user / pn_user empty".into(),
            ));
        }
        let now = chrono::Utc::now().timestamp_millis();
        // First-seen semantics: ON CONFLICT updates the
        // pn_user side but leaves observed_at_ms intact.
        // Caller can re-key a LID to a different PN without
        // bumping the audit stamp.
        sqlx::query(
            "INSERT INTO lid_pn_mappings (tenant_id, lid_user, pn_user, observed_at_ms) \
             VALUES (?,?,?,?) \
             ON CONFLICT(tenant_id, lid_user) DO UPDATE SET pn_user = excluded.pn_user",
        )
        .bind(tenant_id)
        .bind(lid_user)
        .bind(pn_user)
        .bind(now)
        .execute(&self.pool)
        .await?;
        // Read back the persisted row so the returned
        // observed_at_ms reflects the FIRST seen timestamp,
        // not the upsert clock.
        let (observed_at_ms,): (i64,) = sqlx::query_as(
            "SELECT observed_at_ms FROM lid_pn_mappings \
             WHERE tenant_id = ? AND lid_user = ?",
        )
        .bind(tenant_id)
        .bind(lid_user)
        .fetch_one(&self.pool)
        .await?;
        Ok(LidPnMapping {
            tenant_id: TenantIdRef(tenant_id.to_string()),
            lid_user: lid_user.to_string(),
            pn_user: pn_user.to_string(),
            observed_at_ms,
        })
    }

    async fn get_pn_for_lid(
        &self,
        tenant_id: &str,
        lid_user: &str,
    ) -> Result<Option<String>, IdentityError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT pn_user FROM lid_pn_mappings \
             WHERE tenant_id = ? AND lid_user = ?",
        )
        .bind(tenant_id)
        .bind(lid_user)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(p,)| p))
    }

    async fn get_lid_for_pn(
        &self,
        tenant_id: &str,
        pn_user: &str,
    ) -> Result<Option<String>, IdentityError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT lid_user FROM lid_pn_mappings \
             WHERE tenant_id = ? AND pn_user = ?",
        )
        .bind(tenant_id)
        .bind(pn_user)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(l,)| l))
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError> {
        let r = sqlx::query("DELETE FROM lid_pn_mappings WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

/// SQLite-backed company store.
#[derive(Clone)]
pub struct SqliteCompanyStore {
    pool: SqlitePool,
}

impl SqliteCompanyStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[async_trait]
impl CompanyStore for SqliteCompanyStore {
    async fn upsert(&self, tenant_id: &str, company: &Company) -> Result<Company, IdentityError> {
        sqlx::query(
            "INSERT INTO companies \
             (id, tenant_id, domain, name, industry, size_band, \
              enriched_at_ms, is_personal_domain) \
             VALUES (?,?,?,?,?,?,?,?) \
             ON CONFLICT(tenant_id, id) DO UPDATE SET \
              domain=excluded.domain, \
              name=excluded.name, \
              industry=excluded.industry, \
              size_band=excluded.size_band, \
              enriched_at_ms=excluded.enriched_at_ms, \
              is_personal_domain=excluded.is_personal_domain",
        )
        .bind(&company.id.0)
        .bind(tenant_id)
        .bind(&company.domain)
        .bind(&company.name)
        .bind(company.industry.as_deref())
        .bind(company.size_band.as_deref())
        .bind(company.enriched_at_ms)
        .bind(company.is_personal_domain as i64)
        .execute(&self.pool)
        .await?;
        Ok(company.clone())
    }

    async fn get(&self, tenant_id: &str, id: &CompanyId) -> Result<Option<Company>, IdentityError> {
        let row = sqlx::query_as::<_, CompanyRow>(
            "SELECT id, tenant_id, domain, name, industry, size_band, \
                    enriched_at_ms, is_personal_domain \
             FROM companies WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(&id.0)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(CompanyRow::into_company))
    }

    async fn find_by_domain(
        &self,
        tenant_id: &str,
        domain: &str,
    ) -> Result<Option<Company>, IdentityError> {
        let row = sqlx::query_as::<_, CompanyRow>(
            "SELECT id, tenant_id, domain, name, industry, size_band, \
                    enriched_at_ms, is_personal_domain \
             FROM companies WHERE tenant_id = ? AND domain = ?",
        )
        .bind(tenant_id)
        .bind(domain)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(CompanyRow::into_company))
    }

    async fn delete_by_tenant(&self, tenant_id: &str) -> Result<u64, IdentityError> {
        let r = sqlx::query("DELETE FROM companies WHERE tenant_id = ?")
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[derive(Debug, sqlx::FromRow)]
struct CompanyRow {
    id: String,
    tenant_id: String,
    domain: String,
    name: String,
    industry: Option<String>,
    size_band: Option<String>,
    enriched_at_ms: Option<i64>,
    is_personal_domain: i64,
}

impl CompanyRow {
    fn into_company(self) -> Company {
        Company {
            id: CompanyId(self.id),
            tenant_id: TenantIdRef(self.tenant_id),
            domain: self.domain,
            name: self.name,
            industry: self.industry,
            size_band: self.size_band,
            enriched_at_ms: self.enriched_at_ms,
            is_personal_domain: self.is_personal_domain != 0,
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn validate_email(s: &str) -> Result<(), IdentityError> {
    if s.is_empty() || !s.contains('@') || s.len() > 320 {
        return Err(IdentityError::InvalidEmail(s.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::marketing::{Company, CompanyId, EnrichmentStatus, Person, PersonId};

    async fn fresh_pool() -> SqlitePool {
        open_pool(":memory:").await.unwrap()
    }

    fn person_fixture(id: &str, email: &str) -> Person {
        Person {
            id: PersonId(id.into()),
            tenant_id: TenantIdRef("acme".into()),
            primary_name: format!("Person {id}"),
            primary_email: email.into(),
            alt_emails: Vec::new(),
            company_id: None,
            enrichment_status: EnrichmentStatus::None,
            enrichment_confidence: 0.0,
            tags: vec![],
            created_at_ms: 1_700_000_000_000,
            last_seen_at_ms: 1_700_000_000_000,
        }
    }

    fn company_fixture(id: &str, domain: &str) -> Company {
        Company {
            id: CompanyId(id.into()),
            tenant_id: TenantIdRef("acme".into()),
            domain: domain.into(),
            name: format!("Company {id}"),
            industry: Some("saas".into()),
            size_band: Some("10-50".into()),
            enriched_at_ms: Some(1_700_000_000_000),
            is_personal_domain: false,
        }
    }

    #[tokio::test]
    async fn person_upsert_then_get() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        let p = person_fixture("juan", "juan@acme.com");
        store.upsert("acme", &p).await.unwrap();
        let got = store.get("acme", &p.id).await.unwrap().unwrap();
        assert_eq!(got.primary_email, "juan@acme.com");
    }

    #[tokio::test]
    async fn person_find_by_email_returns_none_when_missing() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        let got = store.find_by_email("acme", "ghost@acme.com").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn person_invalid_email_rejected() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        let p = person_fixture("juan", "no-at-symbol");
        let r = store.upsert("acme", &p).await;
        assert!(matches!(r, Err(IdentityError::InvalidEmail(_))));
    }

    #[tokio::test]
    async fn person_tenant_isolation() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        store
            .upsert("acme", &person_fixture("juan", "juan@acme.com"))
            .await
            .unwrap();
        store
            .upsert("globex", &person_fixture("juan", "juan@globex.io"))
            .await
            .unwrap();
        // Same id "juan" exists in BOTH tenants as different
        // rows. Tenant-A query never returns tenant-B's row.
        let got_acme = store
            .get("acme", &PersonId("juan".into()))
            .await
            .unwrap()
            .unwrap();
        let got_globex = store
            .get("globex", &PersonId("juan".into()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got_acme.primary_email, "juan@acme.com");
        assert_eq!(got_globex.primary_email, "juan@globex.io");
    }

    #[tokio::test]
    async fn person_email_add_and_lookup() {
        let pool = fresh_pool().await;
        let store = SqlitePersonEmailStore::new(pool);
        let person_id = PersonId("juan".into());
        store
            .add("acme", &person_id, "juan.alt@gmail.com", true)
            .await
            .unwrap();
        let owner = store
            .find_owner("acme", "juan.alt@gmail.com")
            .await
            .unwrap();
        assert_eq!(owner, Some(person_id));
    }

    #[tokio::test]
    async fn person_email_tenant_isolation() {
        let pool = fresh_pool().await;
        let store = SqlitePersonEmailStore::new(pool);
        // Same email in two tenants — rows coexist.
        store
            .add("acme", &PersonId("juan".into()), "shared@gmail.com", true)
            .await
            .unwrap();
        store
            .add("globex", &PersonId("ana".into()), "shared@gmail.com", true)
            .await
            .unwrap();
        let acme_owner = store.find_owner("acme", "shared@gmail.com").await.unwrap();
        let globex_owner = store
            .find_owner("globex", "shared@gmail.com")
            .await
            .unwrap();
        assert_eq!(acme_owner.unwrap().0, "juan");
        assert_eq!(globex_owner.unwrap().0, "ana");
    }

    #[tokio::test]
    async fn company_upsert_find_by_domain() {
        let pool = fresh_pool().await;
        let store = SqliteCompanyStore::new(pool);
        let c = company_fixture("acme", "acme.com");
        store.upsert("acme", &c).await.unwrap();
        let got = store.find_by_domain("acme", "acme.com").await.unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().name, "Company acme");
    }

    #[tokio::test]
    async fn company_tenant_isolation_same_domain() {
        let pool = fresh_pool().await;
        let store = SqliteCompanyStore::new(pool);
        // Two empresas have a customer at acme.com — the row
        // exists separately per tenant so empresa A's enrichment
        // doesn't bleed to empresa B.
        let c_acme = company_fixture("acme-cust", "acme.com");
        let mut c_globex = company_fixture("acme-cust", "acme.com");
        c_globex.tenant_id = TenantIdRef("globex".into());
        c_globex.industry = Some("fintech".into());
        store.upsert("acme", &c_acme).await.unwrap();
        store.upsert("globex", &c_globex).await.unwrap();
        let acme = store
            .find_by_domain("acme", "acme.com")
            .await
            .unwrap()
            .unwrap();
        let globex = store
            .find_by_domain("globex", "acme.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acme.industry, Some("saas".into()));
        assert_eq!(globex.industry, Some("fintech".into()));
    }

    #[tokio::test]
    async fn delete_by_tenant_cascades_only_for_that_tenant() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool.clone());
        store
            .upsert("acme", &person_fixture("juan", "juan@acme.com"))
            .await
            .unwrap();
        store
            .upsert("globex", &person_fixture("ana", "ana@globex.io"))
            .await
            .unwrap();
        let n = store.delete_by_tenant("acme").await.unwrap();
        assert_eq!(n, 1);
        // Empresa A wiped, empresa B intact.
        assert!(store
            .get("acme", &PersonId("juan".into()))
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get("globex", &PersonId("ana".into()))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn migration_idempotent_on_reopen() {
        // Running open_pool twice on the same in-memory db
        // shouldn't fail — `IF NOT EXISTS` covers it.
        let pool = open_pool(":memory:").await.unwrap();
        sqlx::query(MIGRATION_SQL).execute(&pool).await.unwrap();
        // Confirm tables present.
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('persons','person_emails','person_phones','companies','lid_pn_mappings')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count.0, 5);
    }

    // ─── PersonPhoneStore ──────────────────────────

    async fn fresh_phone_store() -> SqlitePersonPhoneStore {
        SqlitePersonPhoneStore::new(open_pool(":memory:").await.unwrap())
    }

    #[tokio::test]
    async fn phone_add_and_find_owner_round_trips() {
        let store = fresh_phone_store().await;
        let pid = PersonId("juan".into());
        store
            .add("acme", &pid, "+573001234567", true)
            .await
            .unwrap();
        let owner = store.find_owner("acme", "+573001234567").await.unwrap();
        assert_eq!(owner, Some(pid));
    }

    #[tokio::test]
    async fn phone_find_owner_misses_cross_tenant() {
        let store = fresh_phone_store().await;
        store
            .add("acme", &PersonId("juan".into()), "+57300", false)
            .await
            .unwrap();
        let cross = store.find_owner("globex", "+57300").await.unwrap();
        assert!(cross.is_none());
    }

    #[tokio::test]
    async fn phone_add_idempotent_on_conflict() {
        let store = fresh_phone_store().await;
        let pid_old = PersonId("old".into());
        let pid_new = PersonId("new".into());
        store.add("acme", &pid_old, "+57300", false).await.unwrap();
        // Same phone re-added with a different owner — last
        // write wins via ON CONFLICT DO UPDATE.
        store.add("acme", &pid_new, "+57300", true).await.unwrap();
        let owner = store.find_owner("acme", "+57300").await.unwrap();
        assert_eq!(owner, Some(pid_new));
    }

    #[tokio::test]
    async fn phone_list_for_person_returns_all_links() {
        let store = fresh_phone_store().await;
        let pid = PersonId("juan".into());
        for p in &["+573001", "+573002", "+573003"] {
            store.add("acme", &pid, p, false).await.unwrap();
        }
        let rows = store.list_for_person("acme", &pid).await.unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn phone_jid_format_accepted_verbatim() {
        // Caller hasn't normalised yet (E.164 strip).
        // Match is exact — both upsert + lookup must use the
        // same canonical form.
        let store = fresh_phone_store().await;
        let pid = PersonId("juan".into());
        store
            .add("acme", &pid, "573001234567@s.whatsapp.net", false)
            .await
            .unwrap();
        let owner = store
            .find_owner("acme", "573001234567@s.whatsapp.net")
            .await
            .unwrap();
        assert_eq!(owner, Some(pid));
        // E.164 form would NOT match the raw JID.
        let miss = store.find_owner("acme", "+573001234567").await.unwrap();
        assert!(miss.is_none());
    }

    #[tokio::test]
    async fn phone_empty_input_rejected() {
        let store = fresh_phone_store().await;
        let r = store.add("acme", &PersonId("x".into()), "  ", false).await;
        assert!(matches!(r, Err(IdentityError::InvalidEmail(_))));
    }

    #[tokio::test]
    async fn phone_delete_by_tenant_cascades() {
        let store = fresh_phone_store().await;
        store
            .add("acme", &PersonId("a".into()), "+57300", false)
            .await
            .unwrap();
        store
            .add("globex", &PersonId("b".into()), "+12345", false)
            .await
            .unwrap();
        let n = store.delete_by_tenant("acme").await.unwrap();
        assert_eq!(n, 1);
        assert!(store.find_owner("acme", "+57300").await.unwrap().is_none());
        assert_eq!(
            store
                .find_owner("globex", "+12345")
                .await
                .unwrap()
                .map(|p| p.0),
            Some("b".into()),
        );
    }

    // ─── PersonStore::list_by_company ────────

    fn person_with_company(id: &str, email: &str, company: &str) -> Person {
        let mut p = person_fixture(id, email);
        p.company_id = Some(CompanyId(company.into()));
        p
    }

    #[tokio::test]
    async fn list_by_company_returns_matching_rows_only() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        store
            .upsert("acme", &person_with_company("a", "a@globex.io", "globex"))
            .await
            .unwrap();
        store
            .upsert("acme", &person_with_company("b", "b@globex.io", "globex"))
            .await
            .unwrap();
        store
            .upsert("acme", &person_with_company("c", "c@acme.com", "acme"))
            .await
            .unwrap();
        let rows = store.list_by_company("acme", "globex", 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        let ids: Vec<&str> = rows.iter().map(|p| p.id.0.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
    }

    #[tokio::test]
    async fn list_by_company_excludes_null_company() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        // No company_id — must NOT match a `company_id = ?` query.
        store
            .upsert("acme", &person_fixture("a", "a@x.com"))
            .await
            .unwrap();
        store
            .upsert("acme", &person_with_company("b", "b@globex.io", "globex"))
            .await
            .unwrap();
        let rows = store.list_by_company("acme", "globex", 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id.0, "b");
    }

    #[tokio::test]
    async fn list_by_company_is_tenant_scoped() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        store
            .upsert("acme", &person_with_company("a", "a@globex.io", "globex"))
            .await
            .unwrap();
        // Same company id under a different tenant — must
        // NOT leak into the acme query.
        let mut globex_p = person_with_company("g", "g@globex.io", "globex");
        globex_p.tenant_id = TenantIdRef("globex".into());
        store.upsert("globex", &globex_p).await.unwrap();
        let acme_rows = store.list_by_company("acme", "globex", 100).await.unwrap();
        assert_eq!(acme_rows.len(), 1);
        assert_eq!(acme_rows[0].id.0, "a");
    }

    #[tokio::test]
    async fn list_by_company_orders_recent_first() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        let mut older = person_with_company("old", "o@x.io", "globex");
        older.last_seen_at_ms = 1_000;
        let mut newer = person_with_company("new", "n@x.io", "globex");
        newer.last_seen_at_ms = 2_000;
        store.upsert("acme", &older).await.unwrap();
        store.upsert("acme", &newer).await.unwrap();
        let rows = store.list_by_company("acme", "globex", 100).await.unwrap();
        assert_eq!(rows.len(), 2);
        // Most recently seen ranks first.
        assert_eq!(rows[0].id.0, "new");
        assert_eq!(rows[1].id.0, "old");
    }

    #[tokio::test]
    async fn list_by_company_clamps_limit_at_max() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        for i in 0..5 {
            store
                .upsert(
                    "acme",
                    &person_with_company(&format!("p{i}"), &format!("p{i}@x.io"), "globex"),
                )
                .await
                .unwrap();
        }
        // limit=2 caps the result. limit=0 would have used the
        // 1-floor clamp; limit > 1000 would clamp at 1000.
        let rows = store.list_by_company("acme", "globex", 2).await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn list_by_company_unknown_company_returns_empty() {
        let pool = fresh_pool().await;
        let store = SqlitePersonStore::new(pool);
        store
            .upsert("acme", &person_with_company("a", "a@x.io", "globex"))
            .await
            .unwrap();
        let rows = store.list_by_company("acme", "ghost", 100).await.unwrap();
        assert!(rows.is_empty());
    }

    // ─── LidPnMappingStore ─────────────────────

    async fn fresh_lid_pn_store() -> SqliteLidPnMappingStore {
        SqliteLidPnMappingStore::new(open_pool(":memory:").await.unwrap())
    }

    #[tokio::test]
    async fn lid_pn_put_and_get_round_trips_both_directions() {
        let store = fresh_lid_pn_store().await;
        let row = store
            .put("acme", "123456789", "573001234567")
            .await
            .unwrap();
        assert_eq!(row.lid_user, "123456789");
        assert_eq!(row.pn_user, "573001234567");
        // Forward.
        assert_eq!(
            store
                .get_pn_for_lid("acme", "123456789")
                .await
                .unwrap()
                .as_deref(),
            Some("573001234567"),
        );
        // Reverse.
        assert_eq!(
            store
                .get_lid_for_pn("acme", "573001234567")
                .await
                .unwrap()
                .as_deref(),
            Some("123456789"),
        );
    }

    #[tokio::test]
    async fn lid_pn_misses_cross_tenant() {
        let store = fresh_lid_pn_store().await;
        store
            .put("acme", "123456789", "573001234567")
            .await
            .unwrap();
        assert!(store
            .get_pn_for_lid("globex", "123456789")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_lid_for_pn("globex", "573001234567")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn lid_pn_put_first_seen_observed_at_preserved_on_re_upsert() {
        let store = fresh_lid_pn_store().await;
        let first = store.put("acme", "123", "456").await.unwrap();
        // Re-upsert the same lid with a fresh pn — observed_at
        // stays at the first-seen stamp.
        // chrono::now() resolution is millis; sleep so the
        // re-upsert wall clock advances.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = store.put("acme", "123", "789").await.unwrap();
        assert_eq!(second.observed_at_ms, first.observed_at_ms);
        assert_eq!(second.pn_user, "789");
        assert_eq!(
            store
                .get_pn_for_lid("acme", "123")
                .await
                .unwrap()
                .as_deref(),
            Some("789"),
        );
    }

    #[tokio::test]
    async fn lid_pn_unknown_lid_returns_none() {
        let store = fresh_lid_pn_store().await;
        assert!(store
            .get_pn_for_lid("acme", "ghost")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_lid_for_pn("acme", "ghost")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn lid_pn_empty_input_rejected() {
        let store = fresh_lid_pn_store().await;
        assert!(matches!(
            store.put("acme", "", "456").await,
            Err(IdentityError::InvalidEmail(_))
        ));
        assert!(matches!(
            store.put("acme", "123", "  ").await,
            Err(IdentityError::InvalidEmail(_))
        ));
    }

    #[tokio::test]
    async fn lid_pn_delete_by_tenant_cascades() {
        let store = fresh_lid_pn_store().await;
        store.put("acme", "123", "456").await.unwrap();
        store.put("globex", "999", "888").await.unwrap();
        let n = store.delete_by_tenant("acme").await.unwrap();
        assert_eq!(n, 1);
        assert!(store.get_pn_for_lid("acme", "123").await.unwrap().is_none());
        // Globex untouched.
        assert_eq!(
            store
                .get_pn_for_lid("globex", "999")
                .await
                .unwrap()
                .as_deref(),
            Some("888"),
        );
    }

    #[tokio::test]
    async fn lid_pn_distinct_pairs_coexist() {
        let store = fresh_lid_pn_store().await;
        store.put("acme", "lid-a", "pn-a").await.unwrap();
        store.put("acme", "lid-b", "pn-b").await.unwrap();
        assert_eq!(
            store
                .get_pn_for_lid("acme", "lid-a")
                .await
                .unwrap()
                .as_deref(),
            Some("pn-a"),
        );
        assert_eq!(
            store
                .get_pn_for_lid("acme", "lid-b")
                .await
                .unwrap()
                .as_deref(),
            Some("pn-b"),
        );
        assert_eq!(
            store
                .get_lid_for_pn("acme", "pn-a")
                .await
                .unwrap()
                .as_deref(),
            Some("lid-a"),
        );
    }
}
