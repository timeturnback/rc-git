//! Lane assignment for the history graph: linear chains, branches, merges, octopus merges,
//! closing roots, and append stability. Port of `CommitGraphTests.swift`.
#![expect(
    clippy::unwrap_used,
    reason = "the oid helper builds ids from fixed 20-byte seeds"
)]

use std::collections::HashSet;

use rc_git_core::{CommitSummary, Oid};
use rc_git_kit::commit_graph::{GraphBuilder, GraphEdge, GraphRow, MAX_DRAWN_LANES, Phase};

fn oid(seed: u8) -> Oid {
    Oid::from_bytes(&[seed; 20]).unwrap()
}

fn commit(id: Oid, parents: &[Oid]) -> CommitSummary {
    CommitSummary {
        oid: id,
        parents: parents.to_vec(),
        author_name: "Test Author".to_owned(),
        author_email: "author@example.com".to_owned(),
        author_time: 1_704_067_200,
        author_offset: 0,
        committer_time: 1_704_067_200,
        title: "change".to_owned(),
    }
}

fn build(commits: &[CommitSummary]) -> GraphBuilder {
    let mut builder = GraphBuilder::new();
    builder.append(commits);
    builder
}

/// Lanes a row touches: its dot and both ends of every edge.
fn lanes_touched(row: &GraphRow) -> usize {
    let edge_max = row
        .edges
        .iter()
        .flat_map(|edge| [edge.from, edge.to])
        .max()
        .unwrap_or(0);
    row.commit_lane.max(edge_max) + 1
}

fn edge(from: usize, to: usize, color: usize, phase: Phase) -> GraphEdge {
    GraphEdge::new(from, to, color, phase)
}

struct Ids {
    r: Oid,
    a: Oid,
    b: Oid,
    c: Oid,
    m: Oid,
}

fn ids() -> Ids {
    Ids {
        r: oid(0x01),
        a: oid(0x0A),
        b: oid(0x0B),
        c: oid(0x0C),
        m: oid(0x0D),
    }
}

#[test]
fn linear_chain_stays_in_one_lane() {
    let Ids { r, a, b, .. } = ids();
    let builder = build(&[commit(a, &[b]), commit(b, &[r]), commit(r, &[])]);
    let rows = builder.rows();
    assert_eq!(
        rows.iter().map(|row| row.commit_lane).collect::<Vec<_>>(),
        [0, 0, 0]
    );
    assert_eq!(
        rows.iter().map(|row| row.color_index).collect::<Vec<_>>(),
        [0, 0, 0]
    );
    assert_eq!(
        rows.iter().map(lanes_touched).collect::<Vec<_>>(),
        [1, 1, 1]
    );
    assert!(rows.iter().all(|row| !row.is_merge));
    assert_eq!(rows[2].edges, [edge(0, 0, 0, Phase::Into)]);
}

#[test]
fn branch_and_merge_fork_and_rejoin() {
    let Ids { r, a, b, m, .. } = ids();
    let builder = build(&[
        commit(m, &[a, b]),
        commit(a, &[r]),
        commit(b, &[r]),
        commit(r, &[]),
    ]);
    let rows = builder.rows();
    assert_eq!(
        rows.iter().map(|row| row.commit_lane).collect::<Vec<_>>(),
        [0, 0, 1, 0]
    );
    assert_eq!(
        rows.iter().map(|row| row.color_index).collect::<Vec<_>>(),
        [0, 0, 1, 0]
    );
    assert_eq!(
        rows.iter().map(|row| row.is_merge).collect::<Vec<_>>(),
        [true, false, false, false]
    );
    assert_eq!(
        rows.iter().map(lanes_touched).collect::<Vec<_>>(),
        [2, 2, 2, 2]
    );
    assert!(
        rows[1].edges.contains(&edge(1, 1, 1, Phase::Through)),
        "b's lane stays right of a's continuing lane"
    );
    let joins: HashSet<GraphEdge> = rows[3]
        .edges
        .iter()
        .copied()
        .filter(|edge| edge.phase == Phase::Into)
        .collect();
    assert_eq!(
        joins,
        HashSet::from([edge(0, 0, 0, Phase::Into), edge(1, 0, 1, Phase::Into)]),
        "both lanes merge into the shared root"
    );
}

#[test]
fn octopus_merge_opens_a_lane_per_extra_parent() {
    let Ids { a, b, c, m, .. } = ids();
    let builder = build(&[
        commit(m, &[a, b, c]),
        commit(a, &[]),
        commit(b, &[]),
        commit(c, &[]),
    ]);
    let rows = builder.rows();
    assert!(rows[0].is_merge);
    assert_eq!(lanes_touched(&rows[0]), 3);
    assert_eq!(
        rows[0]
            .edges
            .iter()
            .filter(|edge| edge.phase == Phase::Out)
            .count(),
        3
    );
    assert_eq!(
        rows.iter().map(|row| row.commit_lane).collect::<Vec<_>>(),
        [0, 0, 0, 0],
        "each root closes its lane, so the survivors keep sliding left"
    );
    assert_eq!(
        rows.iter().map(|row| row.color_index).collect::<Vec<_>>(),
        [0, 0, 1, 2]
    );
}

#[test]
fn a_closing_root_frees_its_lane() {
    let Ids { a, b, m, .. } = ids();
    let builder = build(&[commit(m, &[a, b]), commit(a, &[]), commit(b, &[])]);
    let rows = builder.rows();
    assert_eq!(rows[1].commit_lane, 0);
    assert!(
        rows[1].edges.contains(&edge(1, 0, 1, Phase::Through)),
        "the surviving lane slides left past the closed one"
    );
    assert_eq!(
        rows[2].commit_lane, 0,
        "the second root lands in the freed lane"
    );
    assert_eq!(rows[2].color_index, 1, "and keeps its own color");
}

#[test]
fn appending_in_pages_never_changes_earlier_rows() {
    let Ids { r, a, b, m, .. } = ids();
    let commits = [
        commit(m, &[a, b]),
        commit(a, &[r]),
        commit(b, &[r]),
        commit(r, &[]),
    ];
    let whole = build(&commits);
    let mut paged = GraphBuilder::new();
    paged.append(&commits[..2]);
    let first_rows = paged.rows().to_vec();
    paged.append(&commits[2..]);
    assert_eq!(paged.rows(), whole.rows(), "two appends equal one build");
    assert_eq!(
        &paged.rows()[..2],
        first_rows.as_slice(),
        "earlier rows never mutate"
    );
    assert_eq!(paged.max_lane_count(), whole.max_lane_count());
}

#[test]
fn a_parent_emitted_before_its_child_opens_no_lane() {
    let m = ids().m;
    let (c1, c2, p, q, y) = (oid(0x21), oid(0x22), oid(0x23), oid(0x24), oid(0x25));
    let builder = build(&[
        commit(m, &[c1, c2]),
        commit(c1, &[p]),
        commit(p, &[q]),
        commit(c2, &[p]),
        commit(q, &[y]),
        commit(y, &[]),
    ]);
    let rows = builder.rows();
    assert_eq!(
        lanes_touched(&rows[4]),
        1,
        "C2's edge to the emitted P ends at its dot"
    );
    assert!(
        rows[4]
            .edges
            .iter()
            .all(|edge| edge.phase != Phase::Through)
    );
    let last = rows.last().unwrap();
    assert!(
        last.edges.iter().all(|edge| edge.phase != Phase::Through),
        "no lane leaks to the end"
    );
}

#[test]
fn a_root_emitted_before_its_child_opens_no_lane() {
    let Ids { a, b, c, m, .. } = ids();
    let unrelated = oid(0x26);
    let builder = build(&[
        commit(m, &[b, c]),
        commit(b, &[a]),
        commit(a, &[]),
        commit(c, &[a]),
        commit(unrelated, &[]),
    ]);
    let last = builder.rows().last().unwrap();
    assert_eq!(lanes_touched(last), 1);
    assert!(last.edges.iter().all(|edge| edge.phase != Phase::Through));
}

#[test]
fn a_merge_parent_already_expected_reuses_its_lane() {
    let Ids { r, a, b, c, m } = ids();
    let x = oid(0x27);
    let builder = build(&[
        commit(x, &[b]),
        commit(m, &[a, b]),
        commit(a, &[r]),
        commit(b, &[r]),
        commit(r, &[]),
    ]);
    let outs: Vec<GraphEdge> = builder.rows()[1]
        .edges
        .iter()
        .copied()
        .filter(|edge| edge.phase == Phase::Out)
        .collect();
    assert_eq!(
        outs,
        [edge(1, 1, 1, Phase::Out), edge(1, 0, 0, Phase::Out)],
        "m's edge to b joins x's lane instead of opening a third"
    );
    assert_eq!(lanes_touched(&builder.rows()[1]), 2);
    assert_eq!(builder.max_lane_count(), 2);

    let (e, d) = (oid(0x28), oid(0x29));
    let nested = build(&[
        commit(e, &[b, d]),
        commit(d, &[c, b]),
        commit(c, &[a]),
        commit(b, &[a]),
        commit(a, &[]),
    ]);
    assert!(nested.rows()[1].edges.contains(&edge(1, 0, 0, Phase::Out)));
    assert_eq!(
        nested.rows().iter().map(lanes_touched).max(),
        Some(2),
        "no third lane opens for b"
    );
}

#[test]
fn drawn_edges_stroke_overflow_lanes_once_in_the_last_column() {
    let edges: Vec<GraphEdge> = (0..30)
        .map(|lane| edge(lane, lane, lane, Phase::Through))
        .collect();
    let row = GraphRow {
        commit_lane: 0,
        color_index: 0,
        edges: edges.clone(),
        is_merge: false,
    };
    let cap = MAX_DRAWN_LANES;
    let drawn = row.drawn_edges(cap);
    assert_eq!(drawn.len(), cap, "one stroke per drawn column");
    assert_eq!(drawn[..cap - 1], edges[..cap - 1], "under the cap");
    assert_eq!(
        drawn.last(),
        Some(&edge(cap - 1, cap - 1, 29, Phase::Through)),
        "the last overflow edge is the one painted on top"
    );
}

#[test]
fn drawn_edges_keep_distinct_segments_in_the_last_column() {
    let row = GraphRow {
        commit_lane: 2,
        color_index: 0,
        edges: vec![
            edge(14, 13, 1, Phase::Through),
            edge(20, 20, 2, Phase::Out),
            edge(15, 15, 3, Phase::Through),
            edge(2, 30, 4, Phase::Out),
        ],
        is_merge: false,
    };
    assert_eq!(
        row.drawn_edges(12),
        [
            edge(11, 11, 2, Phase::Out),
            edge(11, 11, 3, Phase::Through),
            edge(2, 11, 4, Phase::Out),
        ],
        "phase and endpoints tell segments apart; color does not"
    );
}

#[test]
fn drawn_edges_with_a_zero_cap_draw_one_column() {
    let row = GraphRow {
        commit_lane: 0,
        color_index: 0,
        edges: vec![edge(3, 4, 0, Phase::Through)],
        is_merge: false,
    };
    assert_eq!(row.drawn_edges(0), [edge(0, 0, 0, Phase::Through)]);
}

#[test]
fn truncation_leaves_lanes_open() {
    let Ids { a, b, m, .. } = ids();
    let builder = build(&[commit(m, &[a, b])]);
    assert_eq!(
        lanes_touched(&builder.rows()[0]),
        2,
        "both parent lanes stay open past the last row"
    );
    assert_eq!(builder.max_lane_count(), 2);
}

#[test]
fn a_merge_parent_emitted_before_the_merge_opens_no_lane() {
    let Ids { a, m, .. } = ids();
    let early = oid(0x2A);
    let builder = build(&[commit(early, &[]), commit(m, &[a, early]), commit(a, &[])]);
    let rows = builder.rows();
    assert_eq!(
        rows[1]
            .edges
            .iter()
            .filter(|edge| edge.phase == Phase::Out)
            .count(),
        1,
        "only the first parent leaves the merge"
    );
    assert!(
        rows[2]
            .edges
            .iter()
            .all(|edge| edge.phase != Phase::Through)
    );
    assert_eq!(builder.max_lane_count(), 1);
}
