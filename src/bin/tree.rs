#![feature(drain_filter)]
extern crate crater_report_merge;
extern crate failure;
extern crate futures;
extern crate percent_encoding;
extern crate petgraph;
extern crate release_triage;
extern crate reqwest;
extern crate semver;
extern crate serde_json;
extern crate tokio_core;
extern crate url;

use crater_report_merge::{Comparison, TestResults};
use failure::{Error, ResultExt};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::{Direction, Graph};
use release_triage::crates::{Crate, CrateId};
use release_triage::Version;
use std::cmp;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write;
use std::fs::{self, DirEntry};
use std::io;
use std::path::Path;
use std::process::{self, Command};
use std::time::Instant;

fn main() -> Result<(), Error> {
    let mut args = env::args_os().skip(1);
    let result = args.next().unwrap_or_else(|| {
        eprintln!("Provide result file as argument");
        process::exit(1);
    });

    eprintln!("Loading results from {:?}", result);

    let result: TestResults =
        serde_json::from_slice(&fs::read(&result)?).context("failed to parse result file")?;

    let graph = CrateGraph::load();

    let mut crate_ids = Vec::new();
    for (idx, crate_result) in result.crates.iter().enumerate() {
        if crate_result.res != Comparison::Regressed {
            continue;
        }

        let url = &crate_result.url;
        if !url.contains("crates.io") {
            continue;
        }

        let name_start = url.find("crates/").unwrap() + "crates/".len();
        let name_end = name_start + url[name_start..].find("/").unwrap();
        let name = &url[name_start..name_end];

        let version = url[name_end + 1..]
            .parse::<semver::Version>()
            .with_context(|_| format!("failed to parse version: {}", url))?;

        crate_ids.push((
            idx,
            CrateId {
                name: name.into(),
                version: Version::new(version),
            },
        ));
    }

    eprintln!("Finding roots...");

    let crates = crate_ids.clone();
    let _ = crate_ids.drain_filter(|(_, crate_id)| {
        let deps = graph.dependencies_for(crate_id.clone().to_owned());
        for (_, krate) in &crates {
            for dep in &deps {
                if *dep == crate_id {
                    continue;
                }
                if krate == *dep {
                    return true;
                }
            }
        }
        false
    });

    eprintln!("Generating report...");

    let mut report = String::new();

    writeln!(report, "{} root regressions", crate_ids.len())?;

    let mut lines = Vec::new();

    for (idx, id) in &crate_ids {
        let krate = &result.crates[*idx];

        let mut deps = graph.dependencies_for(id.clone().to_owned());
        deps.sort_by_key(|dep| dep.name.as_ref());
        deps.dedup_by_key(|dep| dep.name.as_ref());

        let line = format!(
            " - [{}]({}): [a log]({}/log.txt) vs. [b log]({}/log.txt) ({} dependent crates)",
            krate.name,
            krate.url,
            krate.runs[0].as_ref().unwrap().log,
            krate.runs[1].as_ref().unwrap().log,
            deps.len(),
        );

        lines.push((deps.len(), line));
    }

    lines.sort();
    lines.reverse();

    for (_, line) in &lines {
        writeln!(report, "{}", line)?;
    }

    fs::write("report.md", &report)?;

    Ok(())
}

struct CrateGraph {
    nodes: HashMap<CrateId<'static>, NodeIndex>,
    graph: Graph<CrateId<'static>, ()>,
}

impl CrateGraph {
    fn dependencies_for(&self, id: CrateId<'static>) -> Vec<&CrateId<'static>> {
        let root_node = self.nodes[&id];
        let mut seen = HashSet::new();
        let mut to_process = vec![root_node];
        while let Some(node) = to_process.pop() {
            seen.insert(node);
            // What will break if this node breaks: the incoming edges are from
            // crates that depend on us.
            for edge in self.graph.edges_directed(node, Direction::Incoming) {
                if !seen.contains(&edge.source()) {
                    to_process.push(edge.source());
                }
            }
        }

        seen.into_iter().map(|nid| &self.graph[nid]).collect()
    }

    fn load() -> Self {
        if !Path::new("crates.io-index").exists() {
            run(
                ".",
                "git clone https://github.com/rust-lang/crates.io-index crates.io-index",
            );
        }
        if !run("crates.io-index", "git pull") {
            eprintln!("failed to update index");
        }

        let map = get_all_crates();

        let version_count = map.values().map(|v| v.len()).sum::<usize>();
        let crate_count = map.len();

        println!(
            "loaded {} unique crates and {} versions",
            crate_count, version_count
        );

        let into_graph = Instant::now();

        let mut nodes = HashMap::<CrateId<'static>, _>::with_capacity(version_count);
        let mut graph: Graph<CrateId<'static>, ()> =
            Graph::with_capacity(version_count, version_count);

        let mut latest_release: HashMap<String, Version> = HashMap::new();

        for krate in map.values().flat_map(|v| v) {
            {
                let entry = latest_release
                    .entry(krate.id().name.into_owned())
                    .or_insert_with(|| krate.id().version);
                *entry = cmp::max(entry.clone(), krate.id().version);
            }
            let krate_node = *nodes
                .entry(krate.id().to_owned())
                .or_insert_with(|| graph.add_node(krate.id().to_owned()));
            for dependency in &krate.dependencies {
                let resolutions = map.get(&dependency.name)
                    .or_else(|| map.get(&dependency.name.replace("_", "-")))
                    .unwrap_or_else(|| {
                        panic!("could not find {}", dependency.name);
                    })
                    .iter()
                    .filter(|r| dependency.req.matches(&r.version));
                for resolution in resolutions {
                    let dep_node = *nodes
                        .entry(resolution.id().to_owned())
                        .or_insert_with(|| graph.add_node(resolution.id().to_owned()));
                    graph.update_edge(krate_node.to_owned(), dep_node, ());
                }
            }
        }

        println!(
            "Created crate graph with {} nodes and {} edges in {:?}",
            graph.raw_nodes().len(),
            graph.raw_edges().len(),
            into_graph.elapsed(),
        );

        CrateGraph { nodes, graph }
    }
}

fn run(dir: &str, s: &str) -> bool {
    let mut args = s.split(" ");
    Command::new(args.next().unwrap())
        .current_dir(dir)
        .args(args)
        .status()
        .is_ok()
}

// crate => crates which depend on the key
fn get_all_crates() -> HashMap<String, Vec<Crate>> {
    let mut map = HashMap::new();
    visit_dirs(Path::new("crates.io-index"), &mut |entry| {
        let name = entry.file_name();
        if name.to_string_lossy() == "config.json" {
            return;
        }
        for line in fs::read_to_string(&entry.path()).unwrap().lines() {
            let krate: Crate = serde_json::from_str(&line).unwrap_or_else(|e| {
                panic!("failed to parse {:?}: {:?}", entry.path(), e);
            });
            map.entry(krate.name.clone())
                .or_insert_with(Vec::new)
                .push(krate);
        }
    }).unwrap();
    map
}

fn visit_dirs<F: FnMut(&DirEntry)>(dir: &Path, cb: &mut F) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if !entry.path().to_string_lossy().contains(".git") {
                    visit_dirs(&path, cb)?;
                }
            } else {
                cb(&entry);
            }
        }
    }
    Ok(())
}
