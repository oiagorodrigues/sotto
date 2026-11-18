//! In-memory brute-force cosine search over normalized `f32` vectors (ADR 0004).

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedChunk {
    pub file: String,
    pub ordinal: u32,
    pub text: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit<'a> {
    pub file: &'a str,
    pub ordinal: u32,
    pub text: &'a str,
    pub score: f32,
}

/// Cosine similarity, or `None` for different lengths, empty input and zero-norm vectors.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0_f32, 0_f32, 0_f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 || !dot.is_finite() {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

/// Scales to unit length. `None` for an empty, zero or non-finite vector.
#[must_use]
pub fn unit(v: &[f32]) -> Option<Vec<f32>> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if v.is_empty() || norm == 0.0 || !norm.is_finite() {
        return None;
    }
    Some(v.iter().map(|x| x / norm).collect())
}

/// Vectors are stored at unit length, so a search is one dot product per chunk.
#[derive(Debug, Clone, Default)]
pub struct VectorIndex {
    dimensions: usize,
    entries: Vec<IndexedChunk>,
}

impl VectorIndex {
    #[must_use]
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            entries: Vec::new(),
        }
    }

    /// Returns `false` and stores nothing when the vector has the wrong length or no direction.
    pub fn push(&mut self, mut chunk: IndexedChunk) -> bool {
        if chunk.vector.len() != self.dimensions {
            return false;
        }
        let Some(v) = unit(&chunk.vector) else {
            return false;
        };
        chunk.vector = v;
        self.entries.push(chunk);
        true
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `k` best chunks scoring at least `min_score`, best first. Ties keep file and chunk
    /// order so results are reproducible.
    #[must_use]
    pub fn search(&self, query: &[f32], k: usize, min_score: f32) -> Vec<Hit<'_>> {
        let Some(q) = unit(query).filter(|q| q.len() == self.dimensions) else {
            return Vec::new();
        };
        if k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, &IndexedChunk)> = self
            .entries
            .iter()
            .map(|e| (dot(&q, &e.vector), e))
            .filter(|(s, _)| *s >= min_score)
            .collect();
        let order = |a: &(f32, &IndexedChunk), b: &(f32, &IndexedChunk)| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.file.cmp(&b.1.file))
                .then_with(|| a.1.ordinal.cmp(&b.1.ordinal))
        };
        if scored.len() > k {
            scored.select_nth_unstable_by(k - 1, order);
            scored.truncate(k);
        }
        scored.sort_by(order);
        scored
            .into_iter()
            .map(|(score, e)| Hit {
                file: &e.file,
                ordinal: e.ordinal,
                text: &e.text,
                score,
            })
            .collect()
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(file: &str, ordinal: u32, v: &[f32]) -> IndexedChunk {
        IndexedChunk {
            file: file.into(),
            ordinal,
            text: format!("{file}#{ordinal}"),
            vector: v.to_vec(),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn cosine_basics() {
        assert!(approx(cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap(), 1.0));
        assert!(approx(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap(), 0.0));
        assert!(approx(cosine(&[1.0, 0.0], &[-1.0, 0.0]).unwrap(), -1.0));
        assert!(approx(cosine(&[3.0, 4.0], &[6.0, 8.0]).unwrap(), 1.0));
    }

    #[test]
    fn cosine_rejects_bad_input() {
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), None);
        assert_eq!(cosine(&[], &[]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), None);
        assert_eq!(cosine(&[f32::INFINITY, 1.0], &[1.0, 1.0]), None);
    }

    #[test]
    fn search_orders_by_similarity() {
        let mut idx = VectorIndex::new(2);
        assert!(idx.push(chunk("a", 0, &[1.0, 0.0])));
        assert!(idx.push(chunk("a", 1, &[0.7, 0.7])));
        assert!(idx.push(chunk("b", 0, &[0.0, 1.0])));
        let hits = idx.search(&[1.0, 0.1], 3, -1.0);
        let order: Vec<_> = hits.iter().map(|h| (h.file, h.ordinal)).collect();
        assert_eq!(order, vec![("a", 0), ("a", 1), ("b", 0)]);
        assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
    }

    #[test]
    fn top_k_truncates_and_min_score_filters() {
        let mut idx = VectorIndex::new(2);
        for i in 0..10_u32 {
            let angle = f32::from(u8::try_from(i).unwrap_or(0)) * 0.15;
            idx.push(chunk("f", i, &[angle.cos(), angle.sin()]));
        }
        let top = idx.search(&[1.0, 0.0], 3, -1.0);
        assert_eq!(
            top.iter().map(|h| h.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let strict = idx.search(&[1.0, 0.0], 10, 0.9);
        assert!(strict.len() < 10 && strict.iter().all(|h| h.score >= 0.9));
    }

    #[test]
    fn ties_break_by_file_then_ordinal() {
        let mut idx = VectorIndex::new(2);
        idx.push(chunk("b", 0, &[1.0, 0.0]));
        idx.push(chunk("a", 1, &[1.0, 0.0]));
        idx.push(chunk("a", 0, &[1.0, 0.0]));
        let hits = idx.search(&[1.0, 0.0], 3, 0.0);
        let order: Vec<_> = hits.iter().map(|h| (h.file, h.ordinal)).collect();
        assert_eq!(order, vec![("a", 0), ("a", 1), ("b", 0)]);
    }

    #[test]
    fn wrong_dimensions_and_zero_vectors_are_not_stored() {
        let mut idx = VectorIndex::new(2);
        assert!(!idx.push(chunk("a", 0, &[1.0, 0.0, 0.0])));
        assert!(!idx.push(chunk("a", 1, &[0.0, 0.0])));
        assert!(idx.is_empty());
    }

    #[test]
    fn bad_queries_find_nothing() {
        let mut idx = VectorIndex::new(2);
        idx.push(chunk("a", 0, &[1.0, 0.0]));
        assert!(idx.search(&[1.0], 3, -1.0).is_empty());
        assert!(idx.search(&[0.0, 0.0], 3, -1.0).is_empty());
        assert!(idx.search(&[1.0, 0.0], 0, -1.0).is_empty());
        assert!(VectorIndex::new(2).search(&[1.0, 0.0], 3, -1.0).is_empty());
    }

    #[test]
    fn scores_do_not_depend_on_vector_length() {
        let mut idx = VectorIndex::new(2);
        idx.push(chunk("a", 0, &[100.0, 0.0]));
        let hits = idx.search(&[0.001, 0.0], 1, 0.0);
        assert!(approx(hits[0].score, 1.0));
    }
}
