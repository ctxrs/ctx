use std::path::Path;

use rusqlite::Connection;

mod current;
mod final_v47;
mod legacy;
mod v47_provider_session_repair;
#[cfg(test)]
mod v47_provider_session_repair_tests;

use crate::schema::provider_session_identity::prepare_provider_session_migrations;
use crate::schema::rebuild::finish_result_blob_cleanup;
use crate::schema::scriptgram::migrate_to_v45;
use crate::Result;

use self::current::{migrate_to_v42, migrate_to_v43, migrate_to_v44, migrate_to_v46};
use self::final_v47::{
    migrate_final_v47_provider_source_routes, migrate_final_v47_source_backed_results,
    migrate_final_v47_verified_content_locators,
};
use self::legacy::{
    migrate_to_v1, migrate_to_v10, migrate_to_v11, migrate_to_v12, migrate_to_v13, migrate_to_v14,
    migrate_to_v15, migrate_to_v16, migrate_to_v2, migrate_to_v3, migrate_to_v4, migrate_to_v5,
    migrate_to_v6, migrate_to_v7, migrate_to_v8, migrate_to_v9,
};
use self::v47_provider_session_repair::migrate_to_v47;

pub(crate) fn run_migrations(
    conn: &Connection,
    object_dir: &Path,
    user_version: i64,
) -> Result<()> {
    prepare_provider_session_migrations(conn, user_version)?;
    if user_version < 1 {
        migrate_to_v1(conn)?;
    }
    if user_version < 2 {
        migrate_to_v2(conn)?;
    }
    if user_version < 3 {
        migrate_to_v3(conn)?;
    }
    if user_version < 4 {
        migrate_to_v4(conn)?;
    }
    if user_version < 5 {
        migrate_to_v5(conn)?;
    }
    if user_version < 6 {
        migrate_to_v6(conn)?;
    }
    if user_version < 7 {
        migrate_to_v7(conn)?;
    }
    if user_version < 8 {
        migrate_to_v8(conn)?;
    }
    if user_version < 9 {
        migrate_to_v9(conn)?;
    }
    if user_version < 10 {
        migrate_to_v10(conn)?;
    }
    if user_version < 11 {
        migrate_to_v11(conn)?;
    }
    if user_version < 12 {
        migrate_to_v12(conn)?;
    }
    if user_version < 13 {
        migrate_to_v13(conn)?;
    }
    if user_version < 14 {
        migrate_to_v14(conn)?;
    }
    if user_version < 15 {
        migrate_to_v15(conn)?;
    }
    if user_version < 16 {
        migrate_to_v16(conn)?;
    }
    if user_version < 42 {
        migrate_to_v42(conn)?;
    }
    if user_version < 43 {
        migrate_to_v43(conn)?;
    }
    if user_version < 44 {
        migrate_to_v44(conn, false)?;
    }
    if user_version < 45 {
        migrate_to_v45(conn)?;
    }
    if user_version < 46 {
        migrate_to_v46(conn)?;
    }
    if user_version < 47 {
        migrate_to_v47(conn)?;
    }
    if user_version == 47 {
        migrate_final_v47_source_backed_results(conn)?;
        migrate_final_v47_verified_content_locators(conn)?;
        migrate_final_v47_provider_source_routes(conn)?;
    }
    finish_result_blob_cleanup(conn, object_dir)?;
    Ok(())
}
