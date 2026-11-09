//! dcf-fetch: a .dcf from Figma or from a local document, node-scoped.
//!
//! Same output as DesignCompose's fetch (header id = file name, hidden nodes skipped), but
//! through `Document::new_node_scoped`: one depth=2 request to resolve the root names to node
//! ids, one `/nodes?ids=` request for those subtrees, then /images and /variables; a whole
//! file of 14k nodes takes Figma minutes to answer. With --local the document is the folder
//! fig2json.js wrote from a .fig (`Document::from_local`): no token, no network. The
//! importer's own progress lines (`log::info!`) are printed to stderr.

use clap::Parser;
use dc_bundle::definition_file::{load_design_def, save_design_def};
use dc_bundle::design_compose_definition::DesignComposeDefinitionHeader;
use dc_figma_import::ProxyConfig;
use dc_figma_import::tools::fetch::build_definition;
use dc_figma_import::Document;
use std::time::Instant;

#[derive(Parser, Debug)]
#[clap(about = "A .dcf from Figma or from a local document, node-scoped")]
struct Args {
    /// Figma file key (from the URL). Not with --local.
    #[arg(short, long, required_unless_present = "local")]
    doc_id: Option<String>,
    /// Figma API token (prefer FIGMA_TOKEN in the environment over the command line).
    #[arg(short, long, env = "FIGMA_TOKEN", required_unless_present = "local")]
    api_key: Option<String>,
    /// A local document instead of the Figma API: the folder fig2json.js wrote from a .fig
    /// (file.json, images/<hash>). No token, no network.
    #[arg(long, conflicts_with_all = ["doc_id", "version_id"])]
    local: Option<std::path::PathBuf>,
    /// Root node names to fetch and convert, e.g. "#tv-root".
    #[arg(short, long, required = true)]
    nodes: Vec<String>,
    /// Output .dcf path.
    #[arg(short, long)]
    output: std::path::PathBuf,
    /// Figma version id to pin (optional).
    #[arg(short, long)]
    version_id: Option<String>,
}

/// The document fig2json.js wrote: file.json (the REST /v1/files body) and images/<hash>.
fn local_document(dir: &std::path::Path) -> Result<Document, dc_figma_import::Error> {
    let json = std::fs::read_to_string(dir.join("file.json"))?;
    let mut images = std::collections::HashMap::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("images")) {
        for entry in entries.flatten() {
            let path = entry.path().canonicalize()?;
            if let Some(hash) = path.file_name().and_then(|n| n.to_str()) {
                images.insert(hash.to_string(), Some(format!("file://{}", path.display())));
            }
        }
    }
    let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("local").to_string();
    Document::from_local(id, &json, images)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    let args = Args::parse();
    let started = Instant::now();
    let fetched = match &args.local {
        Some(dir) => local_document(dir),
        None => Document::new_node_scoped(
            args.api_key.as_deref().unwrap_or_default(),
            args.doc_id.clone().unwrap_or_default(),
            args.version_id.clone().unwrap_or_default(),
            &ProxyConfig::None,
            None,
            &args.nodes,
            None,
        ),
    };
    let mut doc = match fetched {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Fetch failed: {e:?}");
            std::process::exit(1);
        }
    };
    log::info!("document fetched in {:.1} s", started.elapsed().as_secs_f32());
    let definition = match build_definition(&mut doc, &args.nodes, true) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Conversion failed: {e:?}");
            std::process::exit(1);
        }
    };
    println!("Fetched document (node-scoped)");
    println!("  DC Version: {}", DesignComposeDefinitionHeader::current_version());
    println!("  Doc ID: {}", doc.get_document_id());
    println!("  Version: {}", doc.get_version());
    println!("  Name: {}", doc.get_name());
    println!("  Last Modified: {}", doc.last_modified());
    let header = DesignComposeDefinitionHeader::current(
        doc.last_modified().clone(),
        doc.get_name().clone(),
        doc.get_version().clone(),
        doc.get_document_id().clone(),
    );
    if let Err(e) = save_design_def(&args.output, &header, &definition) {
        eprintln!("Failed to save {}: {e:?}", args.output.display());
        std::process::exit(1);
    }
    // Like DesignCompose's fetch: the header id becomes the file name, which is what harry's
    // document loader looks up.
    let (mut header, definition) = match load_design_def(&args.output) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to reload {}: {e:?}", args.output.display());
            std::process::exit(1);
        }
    };
    if let Some(name) = args.output.file_name().and_then(|n| n.to_str()) {
        header.id = name.to_string();
    }
    if let Err(e) = save_design_def(&args.output, &header, &definition) {
        eprintln!("Failed to save {}: {e:?}", args.output.display());
        std::process::exit(1);
    }
    println!("Successfully saved document to: {} ({:.1} s)", args.output.display(), started.elapsed().as_secs_f32());
}
