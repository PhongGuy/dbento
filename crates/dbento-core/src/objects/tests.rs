use super::*;
use crate::introspect::RelationRef;

fn users() -> RelationRef {
    RelationRef::new("public", "users")
}

#[test]
fn listings_name_their_objects_by_name() {
    let idx = IndexInfo::new(
        users(),
        "users_email_idx",
        vec!["lower(email)".to_owned()],
        true,
        false,
        true,
        Some("btree".to_owned()),
    );
    assert_eq!(
        idx.object_ref(),
        ObjectRef::Index {
            rel: users(),
            name: "users_email_idx".to_owned()
        }
    );
    assert_eq!(
        (
            idx.name(),
            idx.columns(),
            idx.unique(),
            idx.primary(),
            idx.partial(),
            idx.method()
        ),
        (
            "users_email_idx",
            &["lower(email)".to_owned()][..],
            true,
            false,
            true,
            Some("btree")
        )
    );
    let pk = ConstraintInfo::new(
        users(),
        None,
        ConstraintKind::PrimaryKey,
        vec!["id".to_owned()],
        None,
    );
    assert_eq!(
        pk.object_ref(),
        ObjectRef::Constraint {
            rel: users(),
            name: None,
            kind: ConstraintKind::PrimaryKey
        }
    );
    assert_eq!(
        (pk.name(), pk.kind(), pk.columns()),
        (None, ConstraintKind::PrimaryKey, &["id".to_owned()][..])
    );
    let fk = ConstraintInfo::new(
        users(),
        Some("users_org".to_owned()),
        ConstraintKind::ForeignKey,
        vec!["org_id".to_owned()],
        Some(RelationRef::new("public", "orgs")),
    );
    assert_eq!(
        (fk.name(), fk.referenced()),
        (Some("users_org"), Some(&RelationRef::new("public", "orgs")))
    );
    let trg = TriggerInfo::new(users(), "audit", TriggerState::Disabled);
    assert_eq!((trg.name(), trg.enabled()), ("audit", false));
    assert_eq!(
        trg.object_ref(),
        ObjectRef::Trigger {
            rel: users(),
            name: "audit".to_owned()
        }
    );
    let ty = TypeInfo::new("public", "mood", TypeKind::Enum);
    assert_eq!((ty.name(), ty.kind()), ("mood", TypeKind::Enum));
    assert_eq!(
        ty.object_ref(),
        ObjectRef::Type {
            schema: "public".to_owned(),
            name: "mood".to_owned()
        }
    );
    let f = RoutineInfo::new(
        "public",
        "add",
        RoutineKind::Function,
        "x integer, y integer",
        Some("integer".to_owned()),
    );
    assert_eq!(
        (f.name(), f.kind(), f.arguments(), f.returns()),
        (
            "add",
            RoutineKind::Function,
            "x integer, y integer",
            Some("integer")
        )
    );
    assert_eq!(
        f.object_ref(),
        ObjectRef::Routine {
            schema: "public".to_owned(),
            name: "add".to_owned(),
            arguments: "x integer, y integer".to_owned()
        }
    );
    assert_eq!(ObjectRef::relation(users()), ObjectRef::Relation(users()));
}

#[test]
fn lists_and_definitions_keep_what_they_are_given() {
    let objects = RelationObjects::new(
        Vec::new(),
        Vec::new(),
        vec![TriggerInfo::new(users(), "audit", TriggerState::Enabled)],
    );
    assert_eq!(objects.triggers().len(), 1);
    assert!(objects.indexes().is_empty() && objects.constraints().is_empty());
    let schema = SchemaObjects::new(
        vec![TypeInfo::new("public", "mood", TypeKind::Enum)],
        Vec::new(),
        true,
    );
    assert_eq!(
        (
            schema.types().len(),
            schema.routines().len(),
            schema.truncated()
        ),
        (1, 0, true)
    );
    let d = Definition::new("CREATE INDEX i ON t (a);", false, true);
    assert_eq!(
        (d.text(), d.truncated(), d.composed()),
        ("CREATE INDEX i ON t (a);", false, true)
    );
}

#[test]
fn errors_read_as_the_spec_words_them() {
    let gone = DefinitionError::Missing {
        object: "index i on public.t".to_owned(),
    };
    assert_eq!(gone.to_string(), "index i on public.t no longer exists");
    let no = DefinitionError::Unsupported {
        what: "table public.t".to_owned(),
    };
    assert_eq!(
        no.to_string(),
        "table public.t has no definition on this engine"
    );
    let odd = DefinitionError::Unexpected {
        message: "m".to_owned(),
    };
    assert_eq!(odd.to_string(), "unexpected catalog answer: m");
}

#[test]
fn derives_the_app_relies_on() {
    fn key<T: Clone + std::fmt::Debug + Eq + std::hash::Hash>() {}
    fn kind<T: Copy + Eq + std::hash::Hash>() {}
    fn data<T: Clone + std::fmt::Debug + PartialEq + Send + 'static>() {}
    fn error<T: std::error::Error + Send + Sync + 'static>() {}
    key::<ObjectRef>();
    kind::<ConstraintKind>();
    kind::<TypeKind>();
    kind::<RoutineKind>();
    data::<RelationObjects>();
    data::<SchemaObjects>();
    data::<Definition>();
    error::<DefinitionError>();
}

use crate::introspect::IntrospectError;
use crate::session::ExecuteError;

fn obj() -> ObjectRef {
    ObjectRef::relation(users())
}

#[test]
fn no_text_means_the_object_is_gone() {
    assert!(matches!(
        finish(None, 100, false, &obj()),
        Err(DefinitionError::Missing { object }) if object == "relation public.users"
    ));
}

#[test]
fn a_complete_text_ends_in_exactly_one_semicolon() {
    let d = |t: &str| finish(Some(t.to_owned()), 100, false, &obj()).unwrap();
    assert_eq!(
        d("CREATE INDEX i ON t (a)").text(),
        "CREATE INDEX i ON t (a);"
    );
    assert_eq!(d("SELECT 1;").text(), "SELECT 1;");
    assert_eq!(d("AS $f$SELECT 1$f$\n").text(), "AS $f$SELECT 1$f$;");
    assert!(!d("SELECT 1").truncated() && !d("SELECT 1").composed());
    assert!(
        finish(Some("x".to_owned()), 100, true, &obj())
            .unwrap()
            .composed()
    );
}

#[test]
fn truncation_wins_over_the_semicolon() {
    let cut = finish(Some("abcdefghij".to_owned()), 4, false, &obj()).unwrap();
    assert_eq!((cut.text(), cut.truncated()), ("abcd", true));
    // é is two bytes: a cut inside it backs off to the char boundary.
    let wide = finish(Some("aéé".to_owned()), 4, false, &obj()).unwrap();
    assert_eq!((wide.text(), wide.truncated()), ("aé", true));
    // Fits only without the semicolon: kept whole, marked truncated, never past the cap.
    let edge = finish(Some("abcd".to_owned()), 4, false, &obj()).unwrap();
    assert_eq!((edge.text(), edge.truncated()), ("abcd", true));
    let fits = finish(Some("abc;".to_owned()), 4, false, &obj()).unwrap();
    assert_eq!((fits.text(), fits.truncated()), ("abc;", false));
}

#[test]
fn a_cut_in_trailing_whitespace_is_still_truncated() {
    // The fetch stops past the cap; whitespace after a `;` must not make the cut look whole.
    let cut = finish(Some("ab;   ".to_owned()), 4, false, &obj()).unwrap();
    assert_eq!((cut.text(), cut.truncated()), ("ab;", true));
}

#[test]
fn a_cache_lookup_failure_reads_as_missing_and_other_errors_pass_through() {
    let db = |code: &str, message: &str| {
        IntrospectError::Execute(ExecuteError::Database {
            code: code.to_owned(),
            sqlstate: None,
            message: message.to_owned(),
            detail: None,
            hint: None,
            position: None,
        })
    };
    assert!(matches!(
        definition_error(db("XX000", "cache lookup failed for relation 16384"), &obj()),
        DefinitionError::Missing { object } if object == "relation public.users"
    ));
    assert!(matches!(
        definition_error(db("XX000", "something else"), &obj()),
        DefinitionError::Execute(ExecuteError::Database { .. })
    ));
    assert!(matches!(
        definition_error(db("42501", "permission denied for table pg_proc"), &obj()),
        DefinitionError::Execute(ExecuteError::Database { code, .. }) if code == "42501"
    ));
    assert!(matches!(
        definition_error(IntrospectError::Unexpected { message: "m".to_owned() }, &obj()),
        DefinitionError::Unexpected { message } if message == "m"
    ));
    assert!(matches!(
        definition_error(
            IntrospectError::Execute(ExecuteError::SessionClosed),
            &obj()
        ),
        DefinitionError::Execute(ExecuteError::SessionClosed)
    ));
}

#[test]
fn labels_name_the_kind_and_the_qualified_name() {
    let rel = users();
    assert_eq!(
        ObjectRef::relation(rel.clone()).label(),
        "relation public.users"
    );
    assert_eq!(
        ObjectRef::Index {
            rel: rel.clone(),
            name: "i".to_owned()
        }
        .label(),
        "index i on public.users"
    );
    assert_eq!(
        ObjectRef::Constraint {
            rel: rel.clone(),
            name: None,
            kind: ConstraintKind::PrimaryKey
        }
        .label(),
        "primary key on public.users"
    );
    assert_eq!(
        ObjectRef::Constraint {
            rel: rel.clone(),
            name: Some("c".to_owned()),
            kind: ConstraintKind::Check
        }
        .label(),
        "constraint c on public.users"
    );
    assert_eq!(
        ObjectRef::Trigger {
            rel,
            name: "audit".to_owned()
        }
        .label(),
        "trigger audit on public.users"
    );
    assert_eq!(
        ObjectRef::Type {
            schema: "public".to_owned(),
            name: "mood".to_owned()
        }
        .label(),
        "type public.mood"
    );
    assert_eq!(
        ObjectRef::Routine {
            schema: "public".to_owned(),
            name: "add".to_owned(),
            arguments: "integer, integer".to_owned()
        }
        .label(),
        "routine public.add(integer, integer)"
    );
}
