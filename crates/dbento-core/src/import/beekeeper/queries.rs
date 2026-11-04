//! Turning Beekeeper Studio's `favorite_query` rows into unbound query files, placed under
//! their `query_folder` path with portable names.

use super::db::RawQuery;
use crate::import::model::{ImportSet, ImportedQuery, ItemKind, SkipReason, WarningKind};
use crate::import::names::{file_name, segment};
use std::path::{Path, PathBuf};

/// Adds each query, in the given order, as an unbound file at `<segments>/<file_name>` with
/// origin `<app_db>#favorite_query/<id>`. A null or blank text is skipped as `EmptyQuery`.
pub(crate) fn collect(app_db: &Path, queries: Vec<RawQuery>, set: &mut ImportSet) {
    for q in queries {
        let origin = format!("{}#favorite_query/{}", app_db.display(), q.id);
        // Only valid UTF-8 can be judged blank; bytes that are not UTF-8 are kept as written.
        let Some(text) = q
            .text
            .filter(|b| !std::str::from_utf8(b).is_ok_and(|s| s.trim().is_empty()))
        else {
            set.skip(ItemKind::Query, origin, SkipReason::EmptyQuery);
            continue;
        };
        if q.title_not_utf8 {
            set.warn(&origin, WarningKind::NotUtf8);
        }
        if std::str::from_utf8(&text).is_err() {
            set.warn(&origin, WarningKind::NotUtf8);
        }
        let mut relative: PathBuf = q.folder.iter().map(|f| segment(f)).collect();
        relative.push(file_name(&q.title));
        set.add_query(ImportedQuery::new(None, relative, origin.into(), text));
    }
}
