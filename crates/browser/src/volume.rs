//! (v213) Le volume des écrans : celui des sons de ce navigateur dans Windows.
//!
//! Les pages ne sont plus touchées (un script toutes les deux secondes sur chaque page, qui
//! redonnait aussi son volume au lecteur de YouTube, faisait de petites coupures), et le
//! lecteur de YouTube reste à fond. Chromium joue ses sons depuis un de ses sous-processus
//! (le service audio) : toutes les sessions audio de Windows qui appartiennent à ce
//! navigateur ou à ses descendants prennent le volume voulu, comme le ferait le mélangeur
//! de volume de Windows. Une session qui apparaît (une vidéo qui commence) l'a dans le
//! quart de seconde.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Le volume voulu (0 à 1), partagé avec le fil qui l'applique.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn new(v: f32) -> Self {
        Self(Arc::new(AtomicU32::new(v.clamp(0.0, 1.0).to_bits())))
    }

    pub fn set(&self, v: f32) {
        self.0.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    /// Le fil qui l'applique, tant que le navigateur tourne.
    pub fn start(&self) {
        let v = self.clone();
        let _ = std::thread::Builder::new()
            .name("volume".into())
            .spawn(move || win::run(v));
    }
}

/// Pour cent (0 à 100) vers 0 à 1.
pub fn from_pct(pct: i32) -> f32 {
    pct.clamp(0, 100) as f32 / 100.0
}

/// Ce processus et tous ses descendants, d'après les couples (processus, parent).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn family(me: u32, pairs: &[(u32, u32)]) -> std::collections::HashSet<u32> {
    let mut set = std::collections::HashSet::from([me]);
    loop {
        let n = set.len();
        for &(p, parent) in pairs {
            if p != parent && set.contains(&parent) {
                set.insert(p);
            }
        }
        if set.len() == n {
            return set;
        }
    }
}

#[cfg(windows)]
mod win {
    use super::Volume;
    use windows::core::Interface;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
        ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    pub fn run(v: Volume) {
        // SAFETY : COM pour ce fil seul, appels Windows documentés.
        unsafe {
            if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                eprintln!("volume des écrans : COM indisponible");
                return;
            }
        }
        let mut said = false;
        loop {
            // SAFETY : voir `apply`.
            if let Err(e) = unsafe { apply(v.get()) } {
                if !said {
                    eprintln!("volume des écrans : {e}");
                    said = true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Couples (processus, parent) de tous les processus.
    unsafe fn pairs() -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                out.push((e.th32ProcessID, e.th32ParentProcessID));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        out
    }

    /// Le volume `v` à chaque session audio du navigateur, sur chaque sortie active.
    unsafe fn apply(v: f32) -> windows::core::Result<()> {
        let ours = super::family(std::process::id(), &pairs());
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(i) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(session) = sessions.GetSession(j) else { continue };
                let Ok(control) = session.cast::<IAudioSessionControl2>() else { continue };
                let pid = control.GetProcessId().unwrap_or(0);
                if pid == 0 || !ours.contains(&pid) {
                    continue;
                }
                let Ok(volume) = session.cast::<ISimpleAudioVolume>() else { continue };
                if volume.GetMasterVolume().map_or(true, |now| (now - v).abs() > 0.002) {
                    let _ = volume.SetMasterVolume(v, std::ptr::null());
                }
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod win {
    pub fn run(_v: super::Volume) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_navigateur_et_ses_sous_processus_seulement() {
        // 1 : le système ; 10 : ce navigateur ; 11, 12 : ses sous-processus (12 est le
        // service audio, lancé par 11) ; 20 : un autre programme.
        let pairs = [(1, 0), (10, 1), (11, 10), (12, 11), (20, 1), (21, 20), (5, 5)];
        let f = family(10, &pairs);
        assert_eq!(f, [10, 11, 12].into_iter().collect());
        assert_eq!(from_pct(50), 0.5);
        assert_eq!(from_pct(140), 1.0);
        assert_eq!(from_pct(-3), 0.0);
        let v = Volume::new(0.5);
        v.set(0.25);
        assert_eq!(v.get(), 0.25);
    }
}
