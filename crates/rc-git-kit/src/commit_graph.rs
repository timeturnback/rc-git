//! Port of `CommitGraph.swift`: the lane graph of the paged history, which column each
//! commit's dot sits in and which line segments cross each row.
//!
//! Built strictly in order from [`CommitSummary::parents`], so appending a page never changes
//! rows already built and the pane can render incrementally. The builder only ever sees
//! commits already paged in, so nothing here scales with repository size. Lane indices are
//! unbounded; [`GraphRow::drawn_edges`] clamps them to the drawn width.

use std::collections::{HashMap, HashSet};

use rc_git_core::{CommitSummary, Oid};

/// How a segment relates to the row's commit dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// A lane passing straight through, top edge to bottom edge.
    Through,
    /// A lane ending at the dot: a child connection merging in from the top.
    Into,
    /// A lane starting at the dot: a parent connection leaving toward the bottom.
    Out,
}

/// One line segment crossing a graph row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphEdge {
    /// Lane where the segment starts: at the row's top edge (`Through`, `Into`) or at the dot
    /// (`Out`).
    pub from: usize,
    /// Lane where the segment ends: at the dot (`Into`) or at the row's bottom edge
    /// (`Through`, `Out`).
    pub to: usize,
    /// Index into the palette, append-only: a lane keeps its color as more rows load.
    pub color_index: usize,
    /// How the segment relates to the dot.
    pub phase: Phase,
}

impl GraphEdge {
    /// An edge from its four parts.
    #[must_use]
    pub fn new(from: usize, to: usize, color_index: usize, phase: Phase) -> Self {
        Self {
            from,
            to,
            color_index,
            phase,
        }
    }
}

/// One row of the graph, aligned with one commit of the history list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GraphRow {
    /// Lane of the commit's dot.
    pub commit_lane: usize,
    /// Color of the dot and of the commit's own lane.
    pub color_index: usize,
    /// Every segment drawn across this row.
    pub edges: Vec<GraphEdge>,
    /// Whether the commit has more than one parent.
    pub is_merge: bool,
}

/// Lanes the graph column draws; lanes at or past the last one share its column.
pub const MAX_DRAWN_LANES: usize = 12;

impl GraphRow {
    /// The edges to stroke when lanes at or past `lane_cap - 1` share one column: positions
    /// clamp to that column, and of the edges that then trace the same segment only the last
    /// survives, since it is the one painted on top. Order is otherwise kept, so the result
    /// draws the same picture as `edges` with one stroke per visible segment.
    #[must_use]
    pub fn drawn_edges(&self, lane_cap: usize) -> Vec<GraphEdge> {
        let last = lane_cap.max(1) - 1;
        let clamped: Vec<GraphEdge> = self
            .edges
            .iter()
            .map(|edge| {
                GraphEdge::new(
                    edge.from.min(last),
                    edge.to.min(last),
                    edge.color_index,
                    edge.phase,
                )
            })
            .collect();
        let mut last_index: HashMap<(usize, usize, Phase), usize> = HashMap::new();
        for (index, edge) in clamped.iter().enumerate() {
            last_index.insert((edge.from, edge.to, edge.phase), index);
        }
        let mut drawn = Vec::new();
        for (index, edge) in clamped.iter().enumerate() {
            if last_index.get(&(edge.from, edge.to, edge.phase)) == Some(&index) {
                drawn.push(*edge);
            }
        }
        drawn
    }
}

#[derive(Debug, Clone, Copy)]
struct Lane {
    expected: Oid,
    color_index: usize,
}

/// A lane that survives a row: its index above the row, its index below before the commit's
/// own lane is inserted, and its color.
struct PassThrough {
    top: usize,
    pre: usize,
    color_index: usize,
}

/// Assigns lanes with the active-lanes algorithm: the first lane expecting a commit takes it,
/// other expecting lanes merge in and close, an unexpected commit opens a new rightmost lane.
/// The commit's lane then expects its first parent; other parents reuse a lane already
/// expecting them or open new lanes. Commit-time order is not topological: under committer
/// clock skew a parent can be emitted before one of its children. A parent already emitted
/// gets no lane, since nothing below would ever take it; the child's edge to it ends at the
/// dot.
#[derive(Debug, Clone, Default)]
pub struct GraphBuilder {
    rows: Vec<GraphRow>,
    max_lane_count: usize,
    lanes: Vec<Lane>,
    next_color: usize,
    /// Every commit already given a row; grows only with `rows`.
    emitted: HashSet<Oid>,
}

impl GraphBuilder {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The rows built so far, one per appended commit.
    #[must_use]
    pub fn rows(&self) -> &[GraphRow] {
        &self.rows
    }

    /// The widest the graph has been, for sizing the column without rescanning rows.
    #[must_use]
    pub fn max_lane_count(&self) -> usize {
        self.max_lane_count
    }

    /// Extends the graph with the next commits of the history. Only appends; earlier rows
    /// never change.
    pub fn append(&mut self, commits: &[CommitSummary]) {
        for commit in commits {
            self.append_row(commit);
        }
    }

    fn append_row(&mut self, commit: &CommitSummary) {
        self.emitted.insert(commit.oid);
        let first_parent = commit
            .parents
            .first()
            .copied()
            .filter(|parent| !self.emitted.contains(parent));
        let matching: Vec<usize> = (0..self.lanes.len())
            .filter(|&index| self.lanes[index].expected == commit.oid)
            .collect();
        let top_count = self.lanes.len();
        let (commit_lane, color) = match matching.first() {
            Some(&first) => (first, self.lanes[first].color_index),
            None => (top_count, self.take_color()),
        };

        let mut edges: Vec<GraphEdge> = matching
            .iter()
            .map(|&index| {
                GraphEdge::new(
                    index,
                    commit_lane,
                    self.lanes[index].color_index,
                    Phase::Into,
                )
            })
            .collect();
        let (mut bottom, pass_throughs) = self.close_matching(&matching);
        // The commit's own lane continues to its first parent at the commit's position;
        // survivors at or past that slot shift right by one.
        let slot = commit_lane.min(bottom.len());
        if let Some(parent) = first_parent {
            bottom.insert(
                slot,
                Lane {
                    expected: parent,
                    color_index: color,
                },
            );
            edges.push(GraphEdge::new(commit_lane, slot, color, Phase::Out));
        }
        for pass in pass_throughs {
            let shifted = first_parent.is_some() && pass.pre >= slot;
            let target = if shifted { pass.pre + 1 } else { pass.pre };
            edges.push(GraphEdge::new(
                pass.top,
                target,
                pass.color_index,
                Phase::Through,
            ));
        }
        let others = commit.parents.get(1..).unwrap_or_default();
        self.attach_parents(others, commit_lane, &mut bottom, &mut edges);

        self.lanes = bottom;
        self.max_lane_count = self
            .max_lane_count
            .max(top_count)
            .max(self.lanes.len())
            .max(commit_lane + 1);
        self.rows.push(GraphRow {
            commit_lane,
            color_index: color,
            edges,
            is_merge: commit.parents.len() > 1,
        });
    }

    /// The next color in the append-only sequence.
    fn take_color(&mut self) -> usize {
        let color = self.next_color;
        self.next_color += 1;
        color
    }

    /// Closes every matching lane; survivors keep their order and shift left past them.
    fn close_matching(&self, matching: &[usize]) -> (Vec<Lane>, Vec<PassThrough>) {
        let mut bottom = Vec::new();
        let mut pass_throughs = Vec::new();
        for (index, lane) in self.lanes.iter().enumerate() {
            if matching.contains(&index) {
                continue;
            }
            pass_throughs.push(PassThrough {
                top: index,
                pre: bottom.len(),
                color_index: lane.color_index,
            });
            bottom.push(*lane);
        }
        (bottom, pass_throughs)
    }

    /// Connects the commit to its other parents: a lane already expecting one is reused, and
    /// each remaining parent not yet emitted opens a new rightmost lane.
    fn attach_parents(
        &mut self,
        parents: &[Oid],
        commit_lane: usize,
        bottom: &mut Vec<Lane>,
        edges: &mut Vec<GraphEdge>,
    ) {
        for parent in parents {
            if self.emitted.contains(parent) {
                continue;
            }
            if let Some(existing) = bottom.iter().position(|lane| lane.expected == *parent) {
                let color = bottom[existing].color_index;
                edges.push(GraphEdge::new(commit_lane, existing, color, Phase::Out));
            } else {
                let color = self.take_color();
                bottom.push(Lane {
                    expected: *parent,
                    color_index: color,
                });
                edges.push(GraphEdge::new(
                    commit_lane,
                    bottom.len() - 1,
                    color,
                    Phase::Out,
                ));
            }
        }
    }
}
