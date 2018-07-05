#[macro_use]
extern crate serde_derive;
extern crate futures;
extern crate percent_encoding;
extern crate petgraph;
extern crate release_triage;
extern crate reqwest;
extern crate semver;
extern crate serde;
extern crate serde_json;
extern crate tokio_core;
extern crate url;

use std::cmp::{self, min};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write;
use std::fs::{self, DirEntry, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use release_triage::crates::{Crate, CrateId};
use release_triage::Version;

use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::{Direction, Graph};

macro_rules! string_enum {
    (pub enum $name:ident { $($item:ident => $str:expr,)* }) => {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Copy, Clone)]
        pub enum $name {
            $($item,)*
        }

        impl ::std::str::FromStr for $name {
            type Err = ();

            fn from_str(s: &str) -> Result<$name, ()> {
                Ok(match s {
                    $($str => $name::$item,)*
                    s => panic!("invalid {}: {}", stringify!($name), s),
                })
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                write!(f, "{}", self.to_str())
            }
        }

        impl $name {
            pub fn to_str(&self) -> &'static str {
                match *self {
                    $($name::$item => $str,)*
                }
            }

            pub fn possible_values() -> &'static [&'static str] {
                &[$($str,)*]
            }
        }
    }
}

use reqwest::Client;

fn main() -> Result<(), Box<std::error::Error>> {
    let mut nll: TestResults = serde_json::from_slice(&fs::read("results-nll-1.json")?)?;
    let master: TestResults = serde_json::from_slice(&fs::read("results-pr-51762.json")?)?;

    let mut master_map = HashMap::new();
    for (idx, res) in master.crates.iter().enumerate() {
        assert!(master_map.insert(res.name.clone(), idx).is_none());
    }

    for res in &mut nll.crates {
        if !master_map.contains_key(&res.name) {
            // skip
            continue;
        }
        let master_res = &master.crates[master_map[&res.name]];
        if let Some(result) = &master_res.runs[0] {
            res.runs[0] = Some(result.clone());
            let cmp = match (result.res, res.runs[1].clone().unwrap().res) {
                (TestResult::BuildFail, TestResult::BuildFail) => Comparison::SameBuildFail,
                (TestResult::TestFail, TestResult::TestFail) => Comparison::SameTestFail,
                (TestResult::TestSkipped, TestResult::TestSkipped) => Comparison::SameTestSkipped,
                (TestResult::TestPass, TestResult::TestPass) => Comparison::SameTestPass,
                (TestResult::BuildFail, TestResult::TestFail)
                | (TestResult::BuildFail, TestResult::TestSkipped)
                | (TestResult::BuildFail, TestResult::TestPass)
                | (TestResult::TestFail, TestResult::TestPass) => Comparison::Fixed,
                (TestResult::TestPass, TestResult::TestFail)
                | (TestResult::TestPass, TestResult::BuildFail)
                | (TestResult::TestSkipped, TestResult::BuildFail)
                | (TestResult::TestFail, TestResult::BuildFail) => Comparison::Regressed,
                (TestResult::TestFail, TestResult::TestSkipped)
                | (TestResult::TestPass, TestResult::TestSkipped)
                | (TestResult::TestSkipped, TestResult::TestFail)
                | (TestResult::TestSkipped, TestResult::TestPass) => {
                    panic!("can't compare");
                }
            };
            res.res = cmp;
        }

        if let Some(run) = &mut res.runs[0] {
            run.log = format!(
                "https://cargobomb-reports.s3.amazonaws.com/pr-51762/{}",
                run.log
            );
        }
        if let Some(run) = &mut res.runs[1] {
            run.log = format!(
                "https://cargobomb-reports.s3.amazonaws.com/nll-1/{}",
                run.log
            );
        }
    }

    let mut count = 0;
    for res in &nll.crates {
        if res.res == Comparison::Regressed {
            if let Some(_) = &res.runs[1] {
                let path = PathBuf::from(format!("logs/nll-1/{}", res.name));
                if !path.exists() {
                    count += 1;
                }
            }
        }
    }

    let mut logs = Vec::new();

    let client = Client::new();
    for res in &nll.crates {
        let path = PathBuf::from(format!("logs/nll-1/{}", res.name));
        if path.exists() {
            continue;
        }
        if res.res == Comparison::Regressed {
            if let Some(nll_run) = &res.runs[1] {
                fs::create_dir_all(&path)?;
                let url = format!("{}/log.txt", nll_run.log).replace("+", "%2B");
                let mut response = client.get(&url).send()?;
                if response.status().is_success() {
                    let content = response.text()?;
                    fs::write(path.join("nll.log"), content)?;
                    count -= 1;
                    if count % 100 == 0 {
                        println!("crates left: {}", count);
                    }
                } else {
                    panic!("could not request {}: {:?}", nll_run.log, response);
                }
            }
        }
    }
    for res in &nll.crates {
        let path = PathBuf::from(format!("logs/nll-1/{}", res.name));
        let log = match fs::read_to_string(path.join("nll.log")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !log.contains("internal compiler error") {
            continue;
        }
        logs.push((res, log));
    }
    let mut errors = Vec::new();
    for (res, log) in &logs {
        const ICE: &str = "internal compiler error";
        let error = log.lines()
            .find(|l| l.contains(&format!("{}: ", ICE)))
            .unwrap_or_else(|| {
                panic!("could not find ICE in log for {}", res.name);
            });
        let source = if error.contains("universal_regions.rs:825") {
            let start = error.find(&format!("{}: ", ICE)).unwrap();
            let start = start + format!("{}: ", ICE).len();
            let end = start + error[start..].find(": ").unwrap();
            &error[start..end]
        } else {
            let start = error.find(&format!("{}: ", ICE)).unwrap();
            let start = start + format!("{}: ", ICE).len();
            &error[start..]
        };
        errors.push((source, res.name.clone(), res));
    }

    errors.sort_by_key(|c| c.0.clone());

    let mut current_category = String::new();
    let mut report_md = String::new();
    for (source, _, res) in errors {
        let log_url = format!("{}/log.txt", res.runs[1].as_ref().unwrap().log).replace("+", "%2B");
        let category = source[..min(60, source.len())].to_owned();
        if category != current_category {
            writeln!(report_md, "#### {}", category)?;
            current_category = category;
        }
        writeln!(
            report_md,
            " - [{}]({}): [log]({})",
            res.name, res.url, log_url,
        )?;
    }
    fs::write("report.md", report_md)?;

    println!("nll results: {}", nll.crates.len());
    println!("master results: {}", master.crates.len());

    let out = serde_json::to_string(&nll)?;
    fs::write("results-merged.json", &out)?;

    let graph = CrateGraph::load();

    let dependencies = graph.dependencies_for(CrateId {
        name: String::from("url").into(),
        version: Version::new(semver::Version::from((1, 7, 0))),
    });

    println!("{:?}", dependencies);

    Ok(())
}

struct CrateGraph {
    nodes: HashMap<CrateId<'static>, NodeIndex>,
    graph: Graph<CrateId<'static>, ()>,
    latest_release: HashMap<String, Version>,
}

impl CrateGraph {
    fn dependencies_for(&self, id: CrateId<'static>) -> Vec<String> {
        let root_node = self.nodes[&id];
        let mut versions = HashSet::new();
        let mut crate_names = HashSet::new();
        let mut to_process = vec![root_node];
        while let Some(node) = to_process.pop() {
            let crate_id = &self.graph[node];
            versions.insert(node);
            crate_names.insert(&crate_id.name);
            // What will break if this node breaks: the incoming edges are from
            // crates that depend on us.
            for edge in self.graph.edges_directed(node, Direction::Incoming) {
                if !versions.contains(&edge.source()) {
                    to_process.push(edge.source());
                }
            }
        }

        let mut dependents = versions.iter().collect::<Vec<_>>();
        dependents.sort();
        let dependents = dependents
            .into_iter()
            .map(|p| &self.graph[*p])
            .filter(|p| p.version == self.latest_release[p.name.as_ref()])
            .map(|p| p.to_string())
            .collect::<Vec<_>>();

        if dependents.len() < 20 && env::var_os("QUIET").is_none() {
            println!(
                "dependents on {}: {} crates, {} versions: {:#?}",
                id,
                crate_names.len(),
                dependents.len(),
                dependents
            );
        } else {
            println!(
                "dependents on {}: {} crates, {} versions",
                id,
                crate_names.len(),
                dependents.len()
            );
        }
        dependents
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

        CrateGraph {
            nodes,
            graph,
            latest_release,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct TestResults {
    crates: Vec<CrateResult>,
}

#[derive(Serialize, Deserialize)]
struct CrateResult {
    name: String,
    url: String,
    res: Comparison,
    runs: [Option<BuildTestResult>; 2],
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Comparison {
    Regressed,
    Fixed,
    Skipped,
    Unknown,
    SameBuildFail,
    SameTestFail,
    SameTestSkipped,
    SameTestPass,
}

#[derive(Clone, Serialize, Deserialize)]
struct BuildTestResult {
    res: TestResult,
    log: String,
}

string_enum!(pub enum TestResult {
    BuildFail => "build-fail",
    TestFail => "test-fail",
    TestSkipped => "test-skipped",
    TestPass => "test-pass",
});

fn run(dir: &str, s: &str) -> bool {
    let mut args = s.split(" ");
    Command::new(args.next().unwrap())
        .current_dir(dir)
        .args(args)
        .status()
        .is_ok()
}

/*pub fn _foo_main() {
    let map = get_all_crates();

    let version_count = map.values().map(|v| v.len()).sum::<usize>();
    let crate_count = map.len();

    println!(
        "loaded {} unique crates and {} versions",
        crate_count, version_count
    );

    let mut roots = Vec::new();
    for arg in env::args().skip(1) {
        let space = arg.find(" ").unwrap_or(arg.len());
        let name = &arg[..space];
        let version = &arg[space..].trim();
        let version = VersionReq::parse(version).unwrap_or_else(|e| {
            panic!("{}: failed to parse version req {}: {:?}", name, version, e);
        });
        roots.extend(
            nodes
                .keys()
                .filter(|k| k.name == name && version.matches(&k.version)),
        );
    }
    roots.sort();

    let mut total_broken = HashSet::new();
    let mut total_crates = HashSet::new();
    for root in roots {
        let root_node = nodes[&*root];
        let mut versions = HashSet::new();
        let mut crate_names = HashSet::new();
        let mut to_process = vec![root_node];
        while let Some(node) = to_process.pop() {
            let crate_id = &graph[node];
            versions.insert(node);
            crate_names.insert(&crate_id.name);
            // What will break if this node breaks: the incoming edges are from
            // crates that depend on us.
            for edge in graph.edges_directed(node, Direction::Incoming) {
                if !versions.contains(&edge.source()) {
                    to_process.push(edge.source());
                }
            }
        }

        {
            let mut dependents = versions.iter().collect::<Vec<_>>();
            dependents.sort();
            let dependents = dependents
                .into_iter()
                .map(|p| &graph[*p])
                .filter(|p| p.version == latest_release[&p.name])
                .map(|p| p.to_string())
                .collect::<Vec<_>>();

            if dependents.len() < 20 && env::var_os("QUIET").is_none() {
                println!(
                    "dependents on {}: {} crates, {} versions: {:#?}",
                    root,
                    crate_names.len(),
                    dependents.len(),
                    dependents
                );
            } else {
                println!(
                    "dependents on {}: {} crates, {} versions",
                    root,
                    crate_names.len(),
                    dependents.len()
                );
            }
        }

        total_broken.extend(versions);
        total_crates.extend(crate_names);
    }

    println!(
        "total versions broken: {} ({:.2}%)",
        total_broken.len(),
        ((total_broken.len() as f64) / (graph.raw_nodes().len() as f64)) * 100.0
    );
    println!(
        "total crates broken: {} ({:.2}%)",
        total_crates.len(),
        ((total_crates.len() as f64) / (crate_count as f64)) * 100.0
    );
}*/

// crate => crates which depend on the key
fn get_all_crates() -> HashMap<String, Vec<Crate>> {
    let mut map = HashMap::new();
    visit_dirs(Path::new("crates.io-index"), &mut |entry| {
        let name = entry.file_name();
        if name.to_string_lossy() == "config.json" {
            return;
        }
        for line in read_file(&entry.path()).lines() {
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

fn read_file(path: &Path) -> String {
    let mut file = File::open(path).expect("opened file");
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap_or_else(|e| {
        panic!("failed to read {:?}: {:?}", path, e);
    });
    contents
}
