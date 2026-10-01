use std::{path::Path, sync::Arc};

use gix::progress::DoOrDiscard;
use gix_trace::forest::Tree;
use parking_lot::Mutex;
use rusqlite::params;
use tracing_subscriber::layer::SubscriberExt;

type ProgressItem = DoOrDiscard<gix::progress::prodash::tree::Item>;

pub fn override_thread_subscriber(
    db_path: impl AsRef<Path>,
    progress: Option<ProgressItem>,
    reverse_lines: bool,
) -> anyhow::Result<tracing::subscriber::DefaultGuard> {
    let processor = gix_trace::forest::Printer::new().formatter(StoreTreeToDb {
        con: Arc::new(Mutex::new(rusqlite::Connection::open(&db_path)?)),
        progress: progress.map(Mutex::new),
        reverse_lines,
    });
    let subscriber = tracing_subscriber::Registry::default().with(gix_trace::ForestLayer::from(processor));
    let guard = tracing::subscriber::set_default(subscriber);
    Ok(guard)
}

pub struct StoreTreeToDb {
    con: Arc<Mutex<rusqlite::Connection>>,
    progress: Option<Mutex<ProgressItem>>,
    reverse_lines: bool,
}

impl gix_trace::forest::Formatter for StoreTreeToDb {
    type Error = rusqlite::Error;

    fn fmt(&self, tree: &Tree) -> Result<String, Self::Error> {
        if let Some((progress, tree)) = self
            .progress
            .as_ref()
            .map(Mutex::lock)
            .zip(gix_trace::forest::printer::Pretty.fmt(tree).ok())
        {
            use gix::Progress;
            if self.reverse_lines {
                for line in tree.lines().rev() {
                    progress.info(line.into());
                }
            } else {
                for line in tree.lines() {
                    progress.info(line.into());
                }
            }
        }
        let run_id = tree
            .span()
            .ok()
            .filter(|span| span.name() == "run")
            .and_then(|span| span.fields().iter().find(|field| field.key() == "run_id"))
            .and_then(|field| field.value().parse::<super::db::Id>().ok());
        if let Some(run_id) = run_id {
            let json = serde_json::to_string_pretty(&tree_json(tree)).expect("serialization to string always works");
            self.con
                .lock()
                .execute("UPDATE run SET spans_json = ?1 WHERE id = ?2", params![json, run_id])?;
        }
        Ok(String::new())
    }
}

fn tree_json(tree: &Tree) -> serde_json::Value {
    let fields = |fields: &[gix_trace::forest::tree::Field]| {
        fields
            .iter()
            .map(|field| (field.key().to_owned(), serde_json::Value::from(field.value())))
            .collect::<serde_json::Map<_, _>>()
    };
    match tree {
        Tree::Event(event) => serde_json::json!({
            "Event": {
                "level": event.level().as_str(),
                "fields": fields(event.fields()),
                "message": event.message(),
                "tag": event.tag().map(|tag| tag.to_string()),
            }
        }),
        Tree::Span(span) => serde_json::json!({
            "Span": {
                "level": span.level().as_str(),
                "fields": fields(span.fields()),
                "name": span.name(),
                "nanos_total": span.total_duration().as_nanos(),
                "nanos_nested": span.inner_duration().as_nanos(),
                "nodes": span.nodes().iter().map(tree_json).collect::<Vec<_>>(),
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::db;

    #[test]
    fn delayed_run_spans_keep_their_ids_and_unrelated_roots_only_display() -> anyhow::Result<()> {
        let fixture = tempfile::tempdir()?;
        let db_path = fixture.path().join("run-ids.db");
        let connection = db::create(&db_path)?;
        connection.execute("INSERT INTO run (insertion_time) VALUES (0), (0)", [])?;
        let second_run_id = u32::try_from(connection.last_insert_rowid()).expect("test run id fits in u32");
        let first_run_id = second_run_id - 1;
        {
            let _guard = override_thread_subscriber(&db_path, None, false)?;
            let first = tracing::info_span!("run", run_id = first_run_id);
            first.in_scope(|| tracing::info!("first run"));
            tracing::info_span!("run", run_id = second_run_id).in_scope(|| tracing::info!("second run"));
            drop(first);
            tracing::info!("unrelated event");
            tracing::info_span!("unrelated span", run_id = second_run_id)
                .in_scope(|| tracing::info!("unrelated span event"));
            tracing::info_span!("run").in_scope(|| tracing::info!("run without id"));
        }

        for (run_id, message) in [(first_run_id, "first run"), (second_run_id, "second run")] {
            let stored: Option<String> =
                connection.query_row("SELECT spans_json FROM run WHERE id = ?1", [run_id], |row| row.get(0))?;
            let stored = stored.expect("each completed run span stores its own tree despite closing out of order");
            let stored: serde_json::Value = serde_json::from_str(&stored)?;
            assert_eq!(
                stored["Span"]["fields"]["run_id"],
                run_id.to_string(),
                "completed run spans retain their own ID regardless of closure order"
            );
            assert_eq!(
                stored["Span"]["nodes"][0]["Event"]["message"], message,
                "unrelated roots do not overwrite a run's stored tree"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod serialization_tests {
    use super::*;
    use gix::error::{ErrorExt, message};
    use gix_trace::{
        ForestLayer,
        forest::{Tag, processor},
    };

    #[test]
    fn serialization_preserves_the_corpus_json_shape() -> anyhow::Result<()> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let processor = processor::from_fn(move |tree| {
            sender
                .send(tree)
                .map_err(|err| processor::error(err.0, message("tree receiver dropped").raise()))
        });
        let layer = ForestLayer::new(processor, |event: &tracing::Event<'_>| {
            (event.metadata().target() == "tagged").then(|| {
                Tag::builder()
                    .prefix("request")
                    .level(*event.metadata().level())
                    .build()
            })
        });
        tracing::subscriber::with_default(tracing_subscriber::Registry::default().with(layer), || {
            let root = tracing::info_span!("root", initial = 1, deferred = tracing::field::Empty);
            root.record("initial", 2).record("deferred", "ready");
            root.in_scope(|| {
                tracing::info!(count = 2, "complete");
                let _child = tracing::debug_span!("child").entered();
                tracing::warn!(target: "tagged", "retrying");
            });
        });
        let tree = receiver.try_recv()?;
        let root = tree.span()?;
        let child = root.nodes()[1].span()?;
        let json = serde_json::to_string_pretty(&tree_json(&tree))?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json)?,
            serde_json::json!({
                "Span": {
                    "level": "INFO",
                    "fields": {"initial": "2", "deferred": "\"ready\""},
                    "name": "root",
                    "nanos_total": root.total_duration().as_nanos(),
                    "nanos_nested": child.total_duration().as_nanos(),
                    "nodes": [
                        {"Event": {
                            "level": "INFO",
                            "fields": {"count": "2"},
                            "message": "complete",
                            "tag": null
                        }},
                        {"Span": {
                            "level": "DEBUG",
                            "fields": {},
                            "name": "child",
                            "nanos_total": child.total_duration().as_nanos(),
                            "nanos_nested": 0,
                            "nodes": [{"Event": {
                                "level": "WARN",
                                "fields": {},
                                "message": "retrying",
                                "tag": "request.warn"
                            }}]
                        }}
                    ]
                }
            }),
            "tree variants, final field values, tags, and nanosecond durations retain the stored schema"
        );
        Ok(())
    }
}
