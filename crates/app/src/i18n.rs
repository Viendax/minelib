//! Langues de l'interface. Le jeu est écrit en français ; chaque texte affiché passe par
//! `translate` (branché sur l'interface au démarrage) : un texte connu de la langue choisie
//! est remplacé, les autres restent en français. Un texte « Libellé : valeur » est traduit
//! morceau par morceau.
//!
//! La police du jeu couvre l'alphabet latin (accents de l'Europe de l'Ouest compris) : les
//! langues proposées sont celles qu'elle sait écrire.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Langues proposées : code, nom dans la langue.
pub const LANGS: &[(&str, &str)] = &[
    ("fr", "Français"),
    ("en", "English"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("nl", "Nederlands"),
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Choisit la langue (code inconnu : français).
pub fn set(code: &str) {
    let i = LANGS.iter().position(|l| l.0 == code).unwrap_or(0);
    CURRENT.store(i, Ordering::Relaxed);
}

pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Traduction d'un texte entier dans la langue choisie (`None` : rien à changer).
pub fn translate(text: &str) -> Option<String> {
    let lang = current();
    if lang == 0 {
        return None;
    }
    if let Some(t) = lookup(lang, text) {
        return Some(t.to_string());
    }
    // « Libellé : valeur » : chaque côté traduit s'il est connu.
    let (a, b) = text.split_once(" : ")?;
    let ta = lookup(lang, a);
    let tb = lookup(lang, b);
    if ta.is_none() && tb.is_none() {
        return None;
    }
    let sep = if lang == 0 { " : " } else { ": " };
    Some(format!("{}{sep}{}", ta.unwrap_or(a), tb.unwrap_or(b)))
}

fn lookup(lang: usize, fr: &str) -> Option<&'static str> {
    let row = TABLE.iter().find(|r| r[0] == fr)?;
    row.get(lang).copied().filter(|t| !t.is_empty())
}

/// Français, anglais, espagnol, allemand, italien, portugais, néerlandais.
const TABLE: &[[&str; 7]] = &[
    ["Solo", "Singleplayer", "Un jugador", "Einzelspieler", "Giocatore singolo", "Um jogador", "Singleplayer"],
    ["Multijoueur", "Multiplayer", "Multijugador", "Mehrspieler", "Multigiocatore", "Multijogador", "Multiplayer"],
    ["Paramètres", "Settings", "Ajustes", "Einstellungen", "Impostazioni", "Definições", "Instellingen"],
    ["Quitter le jeu", "Quit game", "Salir del juego", "Spiel beenden", "Esci dal gioco", "Sair do jogo", "Spel afsluiten"],
    ["Compte", "Account", "Cuenta", "Konto", "Account", "Conta", "Account"],
    ["Retour", "Back", "Volver", "Zurück", "Indietro", "Voltar", "Terug"],
    ["Terminé", "Done", "Hecho", "Fertig", "Fatto", "Concluído", "Klaar"],
    ["Annuler", "Cancel", "Cancelar", "Abbrechen", "Annulla", "Cancelar", "Annuleren"],
    ["Valider", "Confirm", "Confirmar", "Bestätigen", "Conferma", "Confirmar", "Bevestigen"],
    ["Actualiser", "Refresh", "Actualizar", "Aktualisieren", "Aggiorna", "Atualizar", "Vernieuwen"],
    ["Rejoindre", "Join", "Unirse", "Beitreten", "Entra", "Entrar", "Deelnemen"],
    ["Jouer", "Play", "Jugar", "Spielen", "Gioca", "Jogar", "Spelen"],
    ["Reprendre", "Resume", "Continuar", "Fortsetzen", "Riprendi", "Continuar", "Hervatten"],
    ["Jeu en pause", "Game paused", "Juego en pausa", "Spiel pausiert", "Gioco in pausa", "Jogo em pausa", "Spel gepauzeerd"],
    ["Quitter la partie", "Leave match", "Salir de la partida", "Spiel verlassen", "Lascia la partita", "Sair da partida", "Wedstrijd verlaten"],
    ["Sauvegarder et quitter", "Save and quit", "Guardar y salir", "Speichern und beenden", "Salva ed esci", "Guardar e sair", "Opslaan en afsluiten"],
    ["Paramètres graphiques", "Graphics settings", "Gráficos", "Grafik", "Grafica", "Gráficos", "Grafische instellingen"],
    ["Régler les touches", "Controls", "Controles", "Steuerung", "Comandi", "Controlos", "Besturing"],
    ["Textures", "Textures", "Texturas", "Texturen", "Texture", "Texturas", "Textures"],
    ["Volume", "Volume", "Volumen", "Lautstärke", "Volume", "Volume", "Volume"],
    ["Langue", "Language", "Idioma", "Sprache", "Lingua", "Idioma", "Taal"],
    ["Skin", "Skin", "Aspecto", "Skin", "Skin", "Skin", "Skin"],
    ["Changer de skin", "Change skin", "Cambiar aspecto", "Skin ändern", "Cambia skin", "Mudar skin", "Skin wijzigen"],
    ["Retirer le skin", "Remove skin", "Quitar aspecto", "Skin entfernen", "Rimuovi skin", "Remover skin", "Skin verwijderen"],
    ["Skin changé.", "Skin changed.", "Aspecto cambiado.", "Skin geändert.", "Skin cambiata.", "Skin alterada.", "Skin gewijzigd."],
    ["Mondes", "Worlds", "Mundos", "Welten", "Mondi", "Mundos", "Werelden"],
    ["Nouveau monde", "New world", "Nuevo mundo", "Neue Welt", "Nuovo mondo", "Novo mundo", "Nieuwe wereld"],
    ["Créer le monde", "Create world", "Crear mundo", "Welt erstellen", "Crea mondo", "Criar mundo", "Wereld maken"],
    ["Nom du monde", "World name", "Nombre del mundo", "Name der Welt", "Nome del mondo", "Nome do mundo", "Naam van de wereld"],
    ["Graine", "Seed", "Semilla", "Startwert", "Seme", "Semente", "Seed"],
    ["Ouvrir le dossier", "Open folder", "Abrir carpeta", "Ordner öffnen", "Apri cartella", "Abrir pasta", "Map openen"],
    ["Ouvrir un monde", "Open a world", "Abrir un mundo", "Welt öffnen", "Apri un mondo", "Abrir um mundo", "Wereld openen"],
    ["Aucun monde pour l'instant : crée-en un !", "No worlds yet: create one!", "Aún no hay mundos: ¡crea uno!", "Noch keine Welt: erstelle eine!", "Nessun mondo: creane uno!", "Ainda sem mundos: cria um!", "Nog geen werelden: maak er een!"],
    ["Se connecter", "Log in", "Iniciar sesión", "Anmelden", "Accedi", "Entrar", "Inloggen"],
    ["Créer un compte", "Create account", "Crear cuenta", "Konto erstellen", "Crea account", "Criar conta", "Account maken"],
    ["Se déconnecter", "Log out", "Cerrar sesión", "Abmelden", "Esci", "Sair", "Uitloggen"],
    ["Jouer hors ligne (solo)", "Play offline (singleplayer)", "Jugar sin conexión", "Offline spielen", "Gioca offline", "Jogar offline", "Offline spelen"],
    ["Double authentification", "Two-factor authentication", "Verificación en dos pasos", "Zwei-Faktor-Anmeldung", "Verifica in due passaggi", "Autenticação de dois fatores", "Tweestapsverificatie"],
    ["Désactiver la double authentification", "Turn off two-factor authentication", "Desactivar la verificación en dos pasos", "Zwei-Faktor-Anmeldung ausschalten", "Disattiva la verifica in due passaggi", "Desativar a autenticação de dois fatores", "Tweestapsverificatie uitzetten"],
    ["Pseudo", "Username", "Nombre", "Name", "Nome", "Nome", "Naam"],
    ["Mot de passe", "Password", "Contraseña", "Passwort", "Password", "Palavra-passe", "Wachtwoord"],
    ["moteur voxel en Rust", "voxel engine in Rust", "motor vóxel en Rust", "Voxel-Engine in Rust", "motore voxel in Rust", "motor voxel em Rust", "voxel-engine in Rust"],
    ["Construction", "Building", "Construcción", "Bauen", "Costruzione", "Construção", "Bouwen"],
    ["FPS - compétitif 5 contre 5", "FPS - competitive 5v5", "FPS - competitivo 5 contra 5", "FPS - Wettkampf 5 gegen 5", "FPS - competitivo 5 contro 5", "FPS - competitivo 5 contra 5", "FPS - competitief 5 tegen 5"],
    ["Serveurs de construction", "Building servers", "Servidores de construcción", "Bau-Server", "Server di costruzione", "Servidores de construção", "Bouwservers"],
    ["Amis", "Friends", "Amigos", "Freunde", "Amici", "Amigos", "Vrienden"],
    ["Top mondial", "World top", "Top mundial", "Weltrangliste", "Top mondiale", "Top mundial", "Wereldtop"],
    ["Carte", "Map", "Mapa", "Karte", "Mappa", "Mapa", "Kaart"],
    ["Chargement du classement…", "Loading leaderboard…", "Cargando clasificación…", "Rangliste wird geladen…", "Caricamento classifica…", "A carregar classificação…", "Ranglijst laden…"],
    ["Ajoute des amis pour comparer vos statistiques.", "Add friends to compare your stats.", "Añade amigos para comparar estadísticas.", "Füge Freunde hinzu, um Statistiken zu vergleichen.", "Aggiungi amici per confrontare le statistiche.", "Adiciona amigos para comparar estatísticas.", "Voeg vrienden toe om statistieken te vergelijken."],
    ["Personne n'a encore fini de partie classée.", "Nobody has finished a ranked match yet.", "Nadie ha terminado aún una partida clasificatoria.", "Noch niemand hat ein Ranglistenspiel beendet.", "Nessuno ha ancora finito una partita classificata.", "Ninguém terminou ainda uma partida classificada.", "Nog niemand heeft een ranked wedstrijd uitgespeeld."],
    ["Aucune carte : crée un monde, règle les départs avec /fps set, et reviens.", "No maps: create a world, set spawns with /fps set, and come back.", "Sin mapas: crea un mundo, define las salidas con /fps set y vuelve.", "Keine Karte: erstelle eine Welt, setze Startpunkte mit /fps set und komm zurück.", "Nessuna mappa: crea un mondo, imposta le partenze con /fps set e torna.", "Sem mapas: cria um mundo, define as partidas com /fps set e volta.", "Geen kaarten: maak een wereld, zet startpunten met /fps set en kom terug."],
    ["hors classement", "unranked", "sin clasificar", "ohne Wertung", "non classificata", "sem classificação", "niet ranked"],
    ["L'hôte lance", "Host starts", "El anfitrión inicia", "Host startet", "L'host avvia", "O anfitrião inicia", "Host start"],
    ["Ta couleur", "Your color", "Tu color", "Deine Farbe", "Il tuo colore", "A tua cor", "Jouw kleur"],
    ["rechercher", "search", "buscar", "suchen", "cerca", "procurar", "zoeken"],
    ["Ajoute un ami avec +", "Add a friend with +", "Añade un amigo con +", "Freund mit + hinzufügen", "Aggiungi un amico con +", "Adiciona um amigo com +", "Voeg een vriend toe met +"],
    ["Aucun ami trouvé", "No friend found", "Ningún amigo", "Kein Freund gefunden", "Nessun amico trovato", "Nenhum amigo", "Geen vriend gevonden"],
    ["inviter", "invite", "invitar", "einladen", "invita", "convidar", "uitnodigen"],
    ["Lobby (clic droit : options)", "Lobby (right-click: options)", "Grupo (clic derecho: opciones)", "Lobby (Rechtsklick: Optionen)", "Lobby (clic destro: opzioni)", "Grupo (clique direito: opções)", "Lobby (rechtsklik: opties)"],
    ["place libre", "free slot", "plaza libre", "freier Platz", "posto libero", "lugar livre", "vrije plek"],
    ["Quitter le lobby", "Leave lobby", "Salir del grupo", "Lobby verlassen", "Lascia la lobby", "Sair do grupo", "Lobby verlaten"],
    ["Exclure", "Kick", "Expulsar", "Entfernen", "Espelli", "Expulsar", "Verwijderen"],
    ["Promouvoir hôte", "Make host", "Hacer anfitrión", "Zum Host machen", "Rendi host", "Tornar anfitrião", "Host maken"],
    ["Accepter", "Accept", "Aceptar", "Annehmen", "Accetta", "Aceitar", "Accepteren"],
    ["Refuser", "Decline", "Rechazar", "Ablehnen", "Rifiuta", "Recusar", "Weigeren"],
    ["Joueur", "Player", "Jugador", "Spieler", "Giocatore", "Jogador", "Speler"],
    ["Matchs", "Matches", "Partidas", "Spiele", "Partite", "Partidas", "Wedstrijden"],
    ["Vict.", "Wins", "Vict.", "Siege", "Vitt.", "Vit.", "Winst"],
    ["Recherche d'une partie…", "Looking for a match…", "Buscando partida…", "Suche nach einem Spiel…", "Ricerca partita…", "À procura de partida…", "Wedstrijd zoeken…"],
    ["Téléchargement de la carte…", "Downloading map…", "Descargando mapa…", "Karte wird geladen…", "Download della mappa…", "A transferir o mapa…", "Kaart downloaden…"],
    ["On rejoint le lobby…", "Joining the lobby…", "Uniéndose al grupo…", "Lobby wird betreten…", "Ingresso nella lobby…", "A entrar no grupo…", "Lobby wordt betreden…"],
    ["Plein écran", "Fullscreen", "Pantalla completa", "Vollbild", "Schermo intero", "Ecrã inteiro", "Volledig scherm"],
    ["oui (F11)", "yes (F11)", "sí (F11)", "ja (F11)", "sì (F11)", "sim (F11)", "ja (F11)"],
    ["non (F11)", "no (F11)", "no (F11)", "nein (F11)", "no (F11)", "não (F11)", "nee (F11)"],
    ["Raytracing (reflets)", "Raytracing (reflections)", "Trazado de rayos (reflejos)", "Raytracing (Spiegelungen)", "Raytracing (riflessi)", "Raytracing (reflexos)", "Raytracing (reflecties)"],
    ["activé", "on", "activado", "an", "attivo", "ativado", "aan"],
    ["désactivé", "off", "desactivado", "aus", "disattivo", "desativado", "uit"],
    ["Limite de FPS", "FPS limit", "Límite de FPS", "FPS-Limit", "Limite FPS", "Limite de FPS", "FPS-limiet"],
    ["illimité", "unlimited", "ilimitado", "unbegrenzt", "illimitato", "ilimitado", "onbeperkt"],
    ["Processeur", "Processor", "Procesador", "Prozessor", "Processore", "Processador", "Processor"],
    ["tous les coeurs", "all cores", "todos los núcleos", "alle Kerne", "tutti i core", "todos os núcleos", "alle kernen"],
    ["coeurs réservés", "reserved cores", "núcleos reservados", "reservierte Kerne", "core riservati", "núcleos reservados", "gereserveerde kernen"],
    ["Mettre à jour", "Update", "Actualizar", "Aktualisieren", "Aggiorna", "Atualizar", "Bijwerken"],
    ["Choisis un pack de textures.", "Choose a texture pack.", "Elige un paquete de texturas.", "Wähle ein Texturenpaket.", "Scegli un pacchetto di texture.", "Escolhe um pacote de texturas.", "Kies een texturepakket."],
    ["Un pseudo et un mot de passe suffisent : ni e-mail, ni numéro.", "A username and a password are enough: no e-mail, no phone number.", "Basta un nombre y una contraseña: sin correo ni teléfono.", "Name und Passwort genügen: keine E-Mail, keine Nummer.", "Bastano nome e password: niente e-mail, niente numero.", "Basta um nome e uma palavra-passe: sem e-mail nem número.", "Een naam en wachtwoord volstaan: geen e-mail, geen nummer."],
    ["Relais", "Relay", "Relé", "Relais", "Relay", "Relé", "Relay"],
    ["Ouvrir un monde en construction en ligne", "Open a world for online building", "Abrir un mundo para construir en línea", "Welt zum Online-Bauen öffnen", "Apri un mondo per costruire online", "Abrir um mundo para construir online", "Wereld openen om online te bouwen"],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_complete_and_parts_translate() {
        let mut seen = std::collections::HashSet::new();
        for r in TABLE {
            assert!(seen.insert(r[0]), "en double : {}", r[0]);
            assert!(r.iter().all(|t| !t.is_empty()), "{r:?}");
        }
        set("en");
        assert_eq!(translate("Jouer").as_deref(), Some("Play"));
        assert_eq!(
            translate("Plein écran : oui (F11)").as_deref(),
            Some("Fullscreen: yes (F11)")
        );
        assert_eq!(translate("texte inconnu"), None);
        set("fr");
        assert_eq!(translate("Jouer"), None);
        set("xx");
        assert_eq!(current(), 0);
    }
}
