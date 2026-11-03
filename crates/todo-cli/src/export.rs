//! `todo export` and `todo import` (design 7.4): the database as JSON
//! Lines, and back into an empty one.

use std::fs;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use todo_daemon::paths::Paths;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::{ExportRequest, ImportRequest};
use tonic_types::StatusExt as _;

use crate::print_err;
use crate::rpc::ready;

/// About how much export text an Import message carries; a single longer
/// line goes on its own.
const IMPORT_BATCH_BYTES: usize = 1 << 20;

/// `todo export [--out PATH] [--no-history]`: to stdout, or to a file
/// written beside PATH and renamed into place. Counts go to stderr.
pub(crate) async fn export(
    paths: &Paths,
    out: Option<PathBuf>,
    no_history: bool,
) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut stream = SystemServiceClient::new(channel)
        .max_decoding_message_size(usize::MAX)
        .export(ExportRequest { no_history })
        .await
        .map_err(|s| anyhow::anyhow!("export failed: {}", s.message()))?
        .into_inner();
    let tmp = out.as_ref().map(|p| p.with_extension("jsonl.tmp"));
    let mut sink: Box<dyn std::io::Write> = match &tmp {
        Some(tmp) => Box::new(std::io::BufWriter::new(
            fs::File::create(tmp).with_context(|| format!("creating {}", tmp.display()))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
    };
    let mut records = 0usize;
    while let Some(batch) = stream
        .message()
        .await
        .map_err(|s| anyhow::anyhow!("export failed: {}", s.message()))?
    {
        for line in batch.lines {
            writeln!(sink, "{line}")?;
            records += 1;
        }
    }
    sink.flush()?;
    drop(sink);
    if let (Some(tmp), Some(out)) = (tmp, &out) {
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, out).with_context(|| format!("writing {}", out.display()))?;
        print_err(format!(
            "exported {} records to {}",
            records.saturating_sub(1),
            out.display()
        ));
    } else {
        print_err(format!("exported {} records", records.saturating_sub(1)));
    }
    Ok(ExitCode::SUCCESS)
}

/// `todo import PATH|-`: into an empty database only.
pub(crate) async fn import(paths: &Paths, from: &Path) -> anyhow::Result<ExitCode> {
    let reader: Box<dyn std::io::BufRead> = if from == Path::new("-") {
        Box::new(std::io::BufReader::new(std::io::stdin()))
    } else {
        Box::new(std::io::BufReader::new(
            fs::File::open(from).with_context(|| format!("reading {}", from.display()))?,
        ))
    };
    let lines: Vec<String> = reader
        .lines()
        .collect::<Result<_, _>>()
        .context("reading the export")?;
    let mut batches: Vec<ImportRequest> = Vec::new();
    let mut size = 0;
    for line in lines {
        match batches.last_mut() {
            Some(batch) if size + line.len() <= IMPORT_BATCH_BYTES => {
                size += line.len();
                batch.lines.push(line);
            }
            _ => {
                size = line.len();
                batches.push(ImportRequest { lines: vec![line] });
            }
        }
    }
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let res = SystemServiceClient::new(channel)
        .max_encoding_message_size(usize::MAX)
        .import(futures_util::stream::iter(batches))
        .await
        .map_err(|s| {
            match s
                .get_details_bad_request()
                .and_then(|b| b.field_violations.into_iter().next())
            {
                Some(v) => anyhow::anyhow!("import failed: {}: {}", v.field, v.description),
                None => anyhow::anyhow!("import failed: {}", s.message()),
            }
        })?
        .into_inner();
    print_err(format!(
        "imported {} records ({} items)",
        res.records, res.items
    ));
    Ok(ExitCode::SUCCESS)
}
