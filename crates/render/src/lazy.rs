//! Valeur longue à préparer (shaders lourds : plusieurs secondes de compilation sous
//! DirectX), calculée sur un fil à part pour ne pas retarder l'ouverture du jeu. Tant
//! qu'elle n'est pas prête, on s'en passe (`try_get`) ou on l'attend (`get`).

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Mutex, OnceLock};

pub struct Lazy<T> {
    cell: OnceLock<T>,
    rx: Mutex<Option<Receiver<T>>>,
}

impl<T: Send + 'static> Lazy<T> {
    /// Lance le calcul sur un fil à part.
    pub fn spawn(name: &str, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let _ = tx.send(f());
            });
        Self {
            cell: OnceLock::new(),
            rx: Mutex::new(Some(rx)),
        }
    }

    /// La valeur si elle est prête (sans attendre).
    pub fn try_get(&self) -> Option<&T> {
        if self.cell.get().is_none() {
            let mut g = self.rx.lock().unwrap();
            if let Some(v) = g.as_ref().and_then(|rx| rx.try_recv().ok()) {
                let _ = self.cell.set(v);
                *g = None;
            }
        }
        self.cell.get()
    }

    /// La valeur, en l'attendant s'il le faut (`None` : le calcul a échoué).
    pub fn get(&self) -> Option<&T> {
        if self.cell.get().is_none() {
            let mut g = self.rx.lock().unwrap();
            if let Some(v) = g.take().and_then(|rx| rx.recv().ok()) {
                let _ = self.cell.set(v);
            }
        }
        self.cell.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_later_without_blocking() {
        let (go, wait) = channel::<()>();
        let l = Lazy::spawn("essai", move || {
            let _ = wait.recv();
            42
        });
        assert_eq!(l.try_get(), None, "pas encore prête, et on n'attend pas");
        go.send(()).unwrap();
        assert_eq!(l.get(), Some(&42));
        assert_eq!(l.try_get(), Some(&42));
    }
}
