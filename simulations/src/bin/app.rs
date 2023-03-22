use std::{path::PathBuf, str::FromStr};

use clap::Parser;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use simulations::{
    config::Config,
    node::{
        carnot::{CarnotNode, CarnotStep},
        Node, NodeId, StepTime,
    },
    overlay::{flat::FlatOverlay, tree::TreeOverlay, Overlay},
    runner::{ConsensusRunner, Report},
};

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path for a yaml-encoded network config file
    config: std::path::PathBuf,
    #[arg(long, default_value_t = OverlayType::Flat)]
    overlay_type: OverlayType,
    #[arg(long, default_value_t = NodeType::Carnot)]
    node_type: NodeType,
    #[arg(short, long, default_value_t = OutputType::StdOut)]
    output: OutputType,
}

#[derive(clap::ValueEnum, Debug, Copy, Clone, Serialize, Deserialize)]
enum OverlayType {
    Flat,
    Tree,
}

impl core::fmt::Display for OverlayType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Flat => write!(f, "flat"),
            Self::Tree => write!(f, "tree"),
        }
    }
}

#[derive(clap::ValueEnum, Debug, Copy, Clone, Serialize, Deserialize)]
enum NodeType {
    Carnot,
}

impl core::fmt::Display for NodeType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Carnot => write!(f, "carnot"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum OutputType {
    File(PathBuf),
    StdOut,
    StdErr,
}

impl core::fmt::Display for OutputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputType::File(path) => write!(f, "{}", path.display()),
            OutputType::StdOut => write!(f, "stdout"),
            OutputType::StdErr => write!(f, "stderr"),
        }
    }
}

impl FromStr for OutputType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stdout" => Ok(Self::StdOut),
            "stderr" => Ok(Self::StdErr),
            path => Ok(Self::File(PathBuf::from(path))),
        }
    }
}

fn run_simulation<N, O>(
    cfg: Config<N::Settings, O::Settings, CarnotStep>,
    node_ids: Vec<NodeId>,
    overlay: O,
) -> Report
where
    N: Node,
    N::Settings: Clone,
    O: Overlay<N>,
{
    let mut rng = thread_rng();
    let layout = overlay.layout(&node_ids, &mut rng);
    let leaders = overlay.leaders(&node_ids, 1, &mut rng).collect();

    let mut runner: ConsensusRunner<N> =
        ConsensusRunner::new(&mut rng, layout, leaders, cfg.node_settings);
    runner.run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap())
        as Box<dyn Fn(&[StepTime]) -> StepTime>)
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Args {
        config,
        overlay_type,
        node_type,
        output,
    } = Args::parse();

    let report = match (overlay_type, node_type) {
        (OverlayType::Flat, NodeType::Carnot) => {
            let cfg = serde_json::from_reader::<
                _,
                Config<
                    <CarnotNode as Node>::Settings,
                    <FlatOverlay as Overlay<CarnotNode>>::Settings,
                    CarnotStep,
                >,
            >(std::fs::File::open(config)?)?;
            #[allow(clippy::unit_arg)]
            let overlay = FlatOverlay::new(cfg.overlay_settings);
            let node_ids = (0..cfg.node_count).collect::<Vec<_>>();
            run_simulation(cfg, node_ids, overlay)
        }
        (OverlayType::Tree, NodeType::Carnot) => {
            let cfg = serde_json::from_reader::<
                _,
                Config<
                    <CarnotNode as Node>::Settings,
                    <TreeOverlay as Overlay<CarnotNode>>::Settings,
                    CarnotStep,
                >,
            >(std::fs::File::open(config)?)?;
            let overlay = TreeOverlay::new(cfg.overlay_settings.clone());
            let node_ids = (0..cfg.node_count).collect::<Vec<_>>();
            run_simulation(cfg, node_ids, overlay)
        }
    };

    let json = serde_json::to_string_pretty(&report)?;
    match output {
        OutputType::File(f) => {
            use std::{fs::OpenOptions, io::Write};

            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(f)?;
            file.write_all(json.as_bytes())?;
        }
        OutputType::StdOut => println!("{json}"),
        OutputType::StdErr => eprintln!("{json}"),
    }
    Ok(())
}
