//! Bilan de la touche « avancer » (v169) : à chaque fin de course, le retard d'« avancer »
//! relâchée sur « courir » relâchée, rangé par durée d'appui, et une ligne au journal toutes
//! les 5 minutes et en fin de partie. Il dit si la touche se comporte encore autrement qu'un
//! clavier normal (voir `stick` pour la touche du portable qui remonte en retard).

use std::time::Instant;

/// Bilan des fins de course (retard d'« avancer » sur « courir ») par durée d'appui, au
/// journal toutes les 5 minutes : il dit si la touche se comporte encore autrement qu'un
/// clavier normal (les deux touches lâchées ensemble remontent à moins de `NORMAL_MS` l'une
/// de l'autre) et à quelle température.
#[derive(Default)]
pub struct LagBook {
    /// (retard en s, durée d'appui en s, température de la carte graphique).
    stops: Vec<(f32, f32, Option<f32>)>,
    since: Option<Instant>,
}

/// Retard (ms) d'un clavier normal : deux touches lâchées ensemble.
pub const NORMAL_MS: f32 = 50.0;
/// Tranches de durée d'appui (s).
const BUCKETS: [(f32, f32, &str); 5] = [
    (0.0, 1.0, "moins d'1 s"),
    (1.0, 3.0, "1 à 3 s"),
    (3.0, 8.0, "3 à 8 s"),
    (8.0, 30.0, "8 à 30 s"),
    (30.0, f32::INFINITY, "plus de 30 s"),
];

impl LagBook {
    pub fn add(&mut self, gap: f32, held: f32, temp: Option<f32>) {
        self.since.get_or_insert_with(Instant::now);
        self.stops.push((gap, held, temp));
    }

    /// Une ligne de bilan toutes les 5 minutes (ou tout de suite : `force`).
    pub fn report(&mut self, force: bool) -> Option<String> {
        let since = self.since?;
        if self.stops.is_empty() || (!force && since.elapsed().as_secs() < 300) {
            return None;
        }
        let mut parts = Vec::new();
        for (lo, hi, name) in BUCKETS {
            let mut gaps: Vec<f32> = self
                .stops
                .iter()
                .filter(|s| s.1 >= lo && s.1 < hi)
                .map(|s| s.0 * 1000.0)
                .collect();
            if gaps.is_empty() {
                continue;
            }
            gaps.sort_by(f32::total_cmp);
            let med = gaps[gaps.len() / 2];
            parts.push(format!(
                "{name} : {} fois, médiane {med:.0} ms, pire {:.0} ms{}",
                gaps.len(),
                gaps[gaps.len() - 1],
                if med < NORMAL_MS { " (comme un clavier normal)" } else { "" }
            ));
        }
        let temps: Vec<f32> = self.stops.iter().filter_map(|s| s.2).collect();
        let temp = if temps.is_empty() {
            String::new()
        } else {
            format!(
                " ; carte graphique en moyenne à {:.0} °C",
                temps.iter().sum::<f32>() / temps.len() as f32
            )
        };
        self.stops.clear();
        self.since = Some(Instant::now());
        Some(format!(
            "touche avancer, bilan (retard sur courir selon la durée d'appui ; clavier normal : \
             moins de {NORMAL_MS:.0} ms) : {}{temp}",
            parts.join(" ; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilan_des_retards_par_duree_d_appui() {
        let mut b = LagBook::default();
        assert_eq!(b.report(true), None);
        b.add(0.03, 0.5, Some(70.0));
        b.add(0.02, 0.8, Some(72.0));
        b.add(0.45, 12.0, Some(88.0));
        let r = b.report(true).unwrap();
        assert!(r.contains("moins d'1 s : 2 fois, médiane 30 ms"), "{r}");
        assert!(r.contains("(comme un clavier normal)"), "{r}");
        assert!(r.contains("8 à 30 s : 1 fois, médiane 450 ms"), "{r}");
        assert!(r.contains("en moyenne à 77 °C"), "{r}");
        assert_eq!(b.report(true), None);
    }
}
