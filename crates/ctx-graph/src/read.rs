use super::*;
use serde::Serialize;

pub(crate) enum ReadCommand {
    Query(QueryArgs),
    Show(SymbolArgs),
    Callers(SymbolArgs),
    Callees(SymbolArgs),
    Impact(ImpactArgs),
    Path(PathArgs),
    Stats,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Output {
    Search(graf::query::SearchResult),
    SearchPath(graf::query::PathSearchResult),
    Graph(GraphResult),
    Path(PathResult),
    Stats(Stats),
    Index(IndexReport),
}

pub(crate) fn options(
    depth: u32,
    limit: u32,
    direction: Direction,
    relation: Option<String>,
) -> Result<QueryOptions> {
    ensure!(depth <= 6, "depth must be between 0 and 6");
    ensure!(
        (1..=500).contains(&limit),
        "limit must be between 1 and 500"
    );
    if let Some(relation) = &relation {
        nonempty(relation, "relation")?;
    }
    Ok(QueryOptions {
        depth,
        limit: limit as usize,
        direction,
        relation,
    })
}

pub(crate) fn nonempty(value: &str, name: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(())
}

pub(crate) fn read(db: &Path, command: ReadCommand) -> Result<Output> {
    let store = Store::open_read_only(db)?;
    let (symbol, options, navigation) = match command {
        ReadCommand::Stats => return Ok(Output::Stats(store.stats()?)),
        ReadCommand::Query(a) => {
            nonempty(&a.text, "text")?;
            let graph = options(a.depth, a.limit, a.direction.into(), a.relation)?;
            let extended = a.navigation.enabled();
            let result = store.query_extended(&a.text, &a.navigation.options(graph))?;
            return Ok(if extended {
                Output::Search(result)
            } else {
                Output::Graph(result.graph)
            });
        }
        ReadCommand::Path(a) => {
            nonempty(&a.source, "source")?;
            nonempty(&a.target, "target")?;
            let extended = a.navigation.enabled();
            let path = store.path_extended(
                &a.source,
                &a.target,
                &a.navigation
                    .options(options(a.depth, a.limit, a.direction.into(), a.relation)?),
            )?;
            if extended {
                return Ok(Output::SearchPath(path));
            }
            return Ok(Output::Path(PathResult {
                found: path.found,
                graph: path.result.graph,
            }));
        }
        ReadCommand::Show(a) => {
            nonempty(&a.symbol, "symbol")?;
            let extended = a.navigation.enabled();
            let options = a
                .navigation
                .options(options(1, a.limit, Direction::Both, None)?);
            let node = store.resolve_endpoint(&a.symbol, &options)?;
            let result = store.neighbors_extended(&node.id, &options)?;
            return Ok(if extended {
                Output::Search(result)
            } else {
                Output::Graph(result.graph)
            });
        }
        ReadCommand::Callers(a) => (
            a.symbol,
            options(1, a.limit, Direction::Incoming, Some("calls".into()))?,
            a.navigation,
        ),
        ReadCommand::Callees(a) => (
            a.symbol,
            options(1, a.limit, Direction::Outgoing, Some("calls".into()))?,
            a.navigation,
        ),
        ReadCommand::Impact(a) => {
            nonempty(&a.symbol, "symbol")?;
            let result = store.impact_extended(
                &a.symbol,
                &graf::query::ImpactOptions {
                    search: a.navigation.options(options(
                        a.depth,
                        a.limit,
                        Direction::Incoming,
                        None,
                    )?),
                    relations: a.relation,
                },
            )?;
            return Ok(Output::Search(result));
        }
    };
    nonempty(&symbol, "symbol")?;
    let extended = navigation.enabled();
    let result = store.neighbors_extended(&symbol, &navigation.options(options))?;
    Ok(if extended {
        Output::Search(result)
    } else {
        Output::Graph(result.graph)
    })
}
