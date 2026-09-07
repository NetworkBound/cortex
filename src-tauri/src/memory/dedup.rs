//! Memory dedup (issue 010 full scope): detect near-identical stored memory
//! chunks via embedding-distance clustering so the Brain UI can flag them for
//! the user to collapse/merge manually. Detection only — nothing here
//! deletes or rewrites a note; that stays a human call.
//!
//! All-local: clusters over embeddings the existing local Ollama indexer
//! already computed (no new embedding calls, no egress). Metadata-only
//! output (paths/scores), matching the precedent set by `brain_stale_notes`.

use crate::memory::embed::cosine;

/// One near-duplicate group: 2+ entries whose pairwise cosine similarity
/// meets or exceeds the clustering threshold, transitively chained (A~B and
/// B~C group A/B/C even if A and C alone fall just under the bar).
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateCluster {
    /// Indices into the input slice, in input order.
    pub members: Vec<usize>,
    /// Highest pairwise cosine similarity observed within the cluster.
    pub max_similarity: f32,
}

/// Tiny union-find (path compression, no union-by-rank — inputs are small
/// enough that it doesn't matter) so transitively-linked near-duplicates land
/// in one cluster instead of fragmenting into overlapping pairs.
struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// Cluster `vectors` by cosine similarity: any pair scoring >= `threshold` is
/// linked (transitively), and every resulting group of size >= 2 is returned
/// as a `DuplicateCluster`, sorted by descending `max_similarity`. O(n^2)
/// pairwise comparisons — fine for the note corpus sizes this indexes (low
/// thousands); callers cap `vectors` if the corpus grows past that.
pub fn cluster_duplicates(vectors: &[Vec<f32>], threshold: f32) -> Vec<DuplicateCluster> {
    let n = vectors.len();
    if n < 2 {
        return Vec::new();
    }
    // Pass 1: union every pair that clears the bar — this is what makes the
    // clustering transitive (A~B and B~C group A/B/C even if A~C alone falls
    // just short).
    let mut dsu = DisjointSet::new(n);
    for i in 0..n {
        for j in (i + 1)..n {
            if cosine(&vectors[i], &vectors[j]) >= threshold {
                dsu.union(i, j);
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        groups.entry(dsu.find(i)).or_default().push(i);
    }
    // Pass 2: recompute each final group's true max pairwise similarity
    // directly from its members — simpler and unambiguously correct than
    // tracking a running max through path-compressing unions, whose "root"
    // key can shift mid-scan.
    let mut clusters: Vec<DuplicateCluster> = groups
        .into_values()
        .filter(|members| members.len() >= 2)
        .map(|members| {
            let mut max_similarity = 0.0f32;
            for a in 0..members.len() {
                for b in (a + 1)..members.len() {
                    let sim = cosine(&vectors[members[a]], &vectors[members[b]]);
                    if sim > max_similarity {
                        max_similarity = sim;
                    }
                }
            }
            DuplicateCluster { members, max_similarity }
        })
        .collect();
    clusters.sort_by(|a, b| b.max_similarity.partial_cmp(&a.max_similarity).unwrap_or(std::cmp::Ordering::Equal));
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicates_below_threshold_yields_no_clusters() {
        let vectors = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![-1.0, 0.0]];
        assert!(cluster_duplicates(&vectors, 0.95).is_empty());
    }

    #[test]
    fn near_identical_pair_clusters_together() {
        // Cosine of these two is > 0.999 — a clear near-duplicate.
        let vectors = vec![vec![1.0, 0.0, 0.0], vec![0.999, 0.001, 0.0], vec![0.0, 1.0, 0.0]];
        let clusters = cluster_duplicates(&vectors, 0.99);
        assert_eq!(clusters.len(), 1, "exactly one cluster expected: {clusters:?}");
        let mut members = clusters[0].members.clone();
        members.sort_unstable();
        assert_eq!(members, vec![0, 1]);
        assert!(clusters[0].max_similarity >= 0.99);
    }

    #[test]
    fn transitive_chain_merges_into_one_cluster() {
        // A~B and B~C both clear the bar, but A~C alone does not — union-find
        // must still place all three in one cluster.
        let a = vec![1.0, 0.0];
        let b = vec![0.995, 0.0998]; // cos(a,b) ~ 0.995
        let c = vec![0.98, 0.199]; // cos(b,c) ~ 0.997, cos(a,c) ~ 0.98
        let vectors = vec![a, b, c];
        let clusters = cluster_duplicates(&vectors, 0.99);
        assert_eq!(clusters.len(), 1, "transitive chain must merge: {clusters:?}");
        let mut members = clusters[0].members.clone();
        members.sort_unstable();
        assert_eq!(members, vec![0, 1, 2]);
    }

    #[test]
    fn singletons_and_empty_input_produce_no_clusters() {
        assert!(cluster_duplicates(&[], 0.9).is_empty());
        assert!(cluster_duplicates(&[vec![1.0, 0.0]], 0.9).is_empty());
    }

    #[test]
    fn clusters_sorted_by_descending_max_similarity() {
        let vectors = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.999, 0.001, 0.0], // near-dup of [0] — very high sim
            vec![0.0, 1.0, 0.0],
            vec![0.02, 0.9998, 0.0], // near-dup of [2] — slightly lower sim
        ];
        let clusters = cluster_duplicates(&vectors, 0.9);
        assert_eq!(clusters.len(), 2);
        assert!(clusters[0].max_similarity >= clusters[1].max_similarity);
    }
}
