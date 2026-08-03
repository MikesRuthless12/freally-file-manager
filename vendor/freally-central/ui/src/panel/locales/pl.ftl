# Freally Central panel — fcp-* strings (pl)
# Loaded into the HOST app's Fluent bundles alongside its own catalogs.
# Keys must stay in sync across all locales (npm run i18n:lint).

fcp-filter-type = Typ
fcp-filter-all = Wszystkie
fcp-available = Dostępne
fcp-coming-soon = Wkrótce
fcp-install-all = Pobierz i zainstaluj wszystko
fcp-install-all-hint = Pobiera i po cichu instaluje wszystkie dostępne aplikacje na ten komputer.
fcp-install-all-unsupported = Instalacja wymaga aplikacji desktopowej Freally Central.
fcp-install-all-none = Brak aplikacji do pobrania.
fcp-detail-back = Wstecz
fcp-detail-features = Funkcje
fcp-detail-started = Rozpoczęto { $date }
fcp-detail-visit-site = Odwiedź stronę
fcp-detail-no-features = Szczegóły funkcji wkrótce.
fcp-detail-coming-soon-note = Ta aplikacja nie jest jeszcze dostępna do pobrania.
fcp-status-offline = Offline — pokazano wbudowany katalog.
fcp-grid-empty = Żadna aplikacja nie pasuje do tego filtra.
fcp-whats-new-title = Nowości
fcp-downloads-count =
    { $count ->
        [one] { $count } pobranie
        [few] { $count } pobrania
        [many] { $count } pobrań
       *[other] { $count } pobrań
    }
fcp-downloads-total = Łącznie
fcp-downloads-brandwide = Aplikacje Freally pobrano { $count } razy
fcp-downloads-unavailable = Liczba pobrań jest teraz niedostępna.
fcp-detail-released = Wydano { $date }
fcp-detail-downloads = Pobrania
fcp-detail-whats-new = Co nowego
fcp-refresh = Odśwież
fcp-changelog-heading = Co nowego w { $name }
fcp-changelog-version = Wersja { $version } · { $date }
fcp-changelog-empty = Brak informacji o wersji.
fcp-changelog-view-full = Zobacz pełny dziennik zmian
fcp-changelog-view-release = Zobacz na GitHub
fcp-card-download = Pobierz
fcp-tagline-freally-capture = Nagrywaj i streamuj jak studio — jedna prosta aplikacja.
fcp-tagline-freally-vault = Twoje hasła, passkeye i sekrety — zaszyfrowane, local-first, Twoje.
fcp-tagline-freally-player = Odtwarza wszystko. Pięknie. Bez reklam, bez spyware.
fcp-tagline-freally-av = Freally Anti-Virus — wkrótce.
fcp-tagline-freally-studio = Studio muzyczne wspomagane AI — wkrótce.
fcp-tagline-freally-sourcerer = Nadchodząca aplikacja Freally.
fcp-tagline-freally-file-manager = Wieloplatformowy menedżer plików — wkrótce.
fcp-desc-freally-capture = Local-first, wieloplatformowe studio streamingu i nagrywania (klasy OBS): kompozytor scen GPU w czasie rzeczywistym, nagrywanie wielościeżkowe i multistreaming na wszystkie platformy naraz — w całości na Twoim komputerze. Bez kont, bez telemetrii, bez chmury.
fcp-desc-freally-vault = Local-first, zero-knowledge, szyfrowany end-to-end menedżer haseł, passkeyów i sekretów. Zgodny z KeePass, bezpieczny pamięciowo rdzeń Rust, całkowicie darmowy — Twój sejf nigdy nie opuszcza Twojego komputera.
fcp-desc-freally-player = Local-first, wieloplatformowy odtwarzacz multimediów, który odtwarza praktycznie wszystko ze sprzętową akceleracją — nowoczesny interfejs, prawdziwa biblioteka multimediów i streaming, bez reklam i bez telemetrii.
fcp-desc-freally-av = Local-first antywirus Freally dla Windows, macOS i Linux. W drodze — jeszcze niedostępny do pobrania.
fcp-desc-freally-studio = W pełni funkcjonalne DAW z generowaniem wzorców AI w ponad 35 gatunkach, mikserem w stylu FL, osią czasu aranżacji i edytorem klipów audio. Przedpremiera — jeszcze niedostępne do pobrania.
fcp-desc-freally-sourcerer = Ta aplikacja Freally jest w drodze. Jeszcze niedostępna do pobrania.
fcp-desc-freally-file-manager = Ta aplikacja Freally jest w drodze. Jeszcze niedostępna do pobrania.
fcp-feat-freally-capture-1 = Własny kompozytor scen GPU (wgpu) z filtrami i przejściami dla każdego źródła
fcp-feat-freally-capture-2 = Nagrywanie wielościeżkowe (sprzętowe enkodery + własny bezstratny kodek)
fcp-feat-freally-capture-3 = Multistreaming na Twitch/YouTube/Kick naraz (RTMP/SRT/WHIP)
fcp-feat-freally-capture-4 = Kamera wirtualna, bufor powtórek, downstream keyery i transformacje 3D
fcp-feat-freally-capture-5 = Interfejs w 18 językach
fcp-status-installed = Zainstalowano
fcp-status-update = Dostępna aktualizacja
fcp-status-not-installed = Niezainstalowano
fcp-action-install = Zainstaluj
fcp-action-update = Aktualizuj
fcp-action-redownload = Pobierz ponownie
fcp-dl-downloading = Pobieranie…
fcp-dl-verifying = Weryfikowanie…
fcp-dl-done-verified = Pobrano i zweryfikowano
fcp-dl-done-size-only = Pobrano (sprawdzono rozmiar)
fcp-dl-failed = Pobieranie nie powiodło się.
fcp-dl-failed-network = Pobieranie nie powiodło się — błąd sieci.
fcp-dl-failed-verify = Weryfikacja nie powiodła się — plik został odrzucony.
fcp-dl-failed-busy = Pobieranie tej aplikacji już trwa.
fcp-dl-canceled = Pobieranie anulowane.
fcp-dl-cancel = Anuluj
fcp-dl-retry = Spróbuj ponownie
fcp-dl-progress-label = Postęp pobierania { $name }
fcp-dl-show-in-folder = Pokaż w folderze
fcp-batch-progress = Pobrano { $done } z { $total }
fcp-batch-cancel-all = Anuluj wszystko
fcp-batch-done = Wszystkie pobrania ukończone.
fcp-batch-failed = Nie powiodło się { $failed } z { $total } pobrań.
fcp-batch-canceled = Anulowano { $canceled } z { $total } pobrań.
fcp-dl-installing = Instalowanie…
fcp-install-progress-label = Postęp instalacji { $name }
fcp-install-done = Zainstalowano ✓
fcp-install-canceled = Instalacja anulowana.
fcp-install-failed = Instalacja nie powiodła się.
fcp-install-failed-not-downloaded = Najpierw pobierz aplikację, aby ją zainstalować.
fcp-install-failed-not-verified = Ten instalator nie jest zweryfikowany do cichej instalacji — nie został uruchomiony.
fcp-install-failed-verify = Weryfikacja nie powiodła się — instalator został odrzucony.
fcp-install-failed-unsupported = Tego typu instalatora nie można zainstalować po cichu na tym komputerze.
fcp-install-failed-installer = Instalator zgłosił błąd.
fcp-install-failed-elevation = Odmówiono uprawnień administratora.
fcp-install-failed-busy = Instalacja już trwa.
fcp-open-app = Otwórz
fcp-open-failed = Nie można otworzyć { $name }.
fcp-batch-installing = Instalowanie… zainstalowano { $done } z { $total }
fcp-batch-installed = Wszystkie aplikacje zainstalowane.
fcp-batch-install-failed = Nie powiodło się { $failed } z { $total } instalacji.
fcp-batch-install-canceled = Anulowano { $canceled } z { $total } instalacji.
fcp-modal-close = Zamknij
