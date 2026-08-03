# Freally Central panel — fcp-* strings (uk)
# Loaded into the HOST app's Fluent bundles alongside its own catalogs.
# Keys must stay in sync across all locales (npm run i18n:lint).

fcp-filter-type = Тип
fcp-filter-all = Усі
fcp-available = Доступні
fcp-coming-soon = Незабаром
fcp-install-all = Завантажити й установити все
fcp-install-all-hint = Завантажує та тихо встановлює всі доступні застосунки для цього комп’ютера.
fcp-install-all-unsupported = Для встановлення потрібен настільний застосунок Freally Central.
fcp-install-all-none = Поки немає застосунків для завантаження.
fcp-detail-back = Назад
fcp-detail-features = Можливості
fcp-detail-started = Розпочато { $date }
fcp-detail-visit-site = Відкрити сайт
fcp-detail-no-features = Деталі можливостей незабаром.
fcp-detail-coming-soon-note = Цей додаток ще недоступний для завантаження.
fcp-status-offline = Офлайн — показано вбудований каталог.
fcp-grid-empty = Жоден застосунок не відповідає цьому фільтру.
fcp-whats-new-title = Що нового
fcp-downloads-count =
    { $count ->
        [one] { $count } завантаження
        [few] { $count } завантаження
        [many] { $count } завантажень
       *[other] { $count } завантаження
    }
fcp-downloads-total = Усього
fcp-downloads-brandwide = Застосунки Freally завантажено { $count } разів
fcp-downloads-unavailable = Кількість завантажень зараз недоступна.
fcp-detail-released = Випущено { $date }
fcp-detail-downloads = Завантаження
fcp-detail-whats-new = Що нового
fcp-refresh = Оновити
fcp-changelog-heading = Що нового у { $name }
fcp-changelog-version = Версія { $version } · { $date }
fcp-changelog-empty = Ще немає приміток до випуску.
fcp-changelog-view-full = Переглянути повний журнал змін
fcp-changelog-view-release = Переглянути на GitHub
fcp-card-download = Завантажити
fcp-tagline-freally-capture = Записуйте та стримте як студія — один зручний застосунок.
fcp-tagline-freally-vault = Ваші паролі, passkey та секрети — зашифровані, local-first, ваші.
fcp-tagline-freally-player = Відтворює все. Красиво. Без реклами та шпигунів.
fcp-tagline-freally-av = Freally Anti-Virus — незабаром.
fcp-tagline-freally-studio = Музична студія з підтримкою ШІ — незабаром.
fcp-tagline-freally-sourcerer = Майбутній застосунок Freally.
fcp-tagline-freally-file-manager = Кросплатформний файловий менеджер — незабаром.
fcp-desc-freally-capture = Локальна кросплатформна студія трансляцій і запису (рівня OBS): GPU-композитор сцен у реальному часі, багатодоріжковий запис і мультистрим на всі платформи одночасно — повністю на вашому комп'ютері. Без облікових записів, без телеметрії, без хмари.
fcp-desc-freally-vault = Локальний, zero-knowledge, наскрізно зашифрований менеджер паролів, passkey та секретів. Сумісний із KeePass, безпечне для пам'яті ядро на Rust, повністю безкоштовний — ваше сховище ніколи не покидає ваш комп'ютер.
fcp-desc-freally-player = Локальний кросплатформний медіаплеєр, що відтворює майже все з апаратним прискоренням — сучасний інтерфейс, справжня медіатека та стримінг, без реклами й телеметрії.
fcp-desc-freally-av = Локальний антивірус Freally для Windows, macOS і Linux. У розробці — ще недоступний для завантаження.
fcp-desc-freally-studio = Повнофункціональна DAW з ШІ-генерацією патернів у понад 35 жанрах, мікшером у стилі FL, таймлайном аранжування та редактором аудіокліпів. Передрелізна версія — ще недоступна для завантаження.
fcp-desc-freally-sourcerer = Цей застосунок Freally у розробці. Ще недоступний для завантаження.
fcp-desc-freally-file-manager = Цей застосунок Freally у розробці. Ще недоступний для завантаження.
fcp-feat-freally-capture-1 = Власний GPU-композитор сцен (wgpu) з фільтрами та переходами для кожного джерела
fcp-feat-freally-capture-2 = Багатодоріжковий запис (апаратні кодувальники + власний кодек без втрат)
fcp-feat-freally-capture-3 = Мультистрим на Twitch/YouTube/Kick одночасно (RTMP/SRT/WHIP)
fcp-feat-freally-capture-4 = Віртуальна камера, буфер повторів, downstream-кеєри та 3D-трансформації
fcp-feat-freally-capture-5 = Інтерфейс 18 мовами
fcp-status-installed = Встановлено
fcp-status-update = Доступне оновлення
fcp-status-not-installed = Не встановлено
fcp-action-install = Встановити
fcp-action-update = Оновити
fcp-action-redownload = Завантажити знову
fcp-dl-downloading = Завантаження…
fcp-dl-verifying = Перевірка…
fcp-dl-done-verified = Завантажено й перевірено
fcp-dl-done-size-only = Завантажено (розмір звірено)
fcp-dl-failed = Не вдалося завантажити.
fcp-dl-failed-network = Не вдалося завантажити — помилка мережі.
fcp-dl-failed-verify = Перевірка не пройдена — файл відхилено.
fcp-dl-failed-busy = Для цього застосунку вже триває завантаження.
fcp-dl-canceled = Завантаження скасовано.
fcp-dl-cancel = Скасувати
fcp-dl-retry = Повторити
fcp-dl-progress-label = Перебіг завантаження { $name }
fcp-dl-show-in-folder = Показати в папці
fcp-batch-progress = Завантажено { $done } з { $total }
fcp-batch-cancel-all = Скасувати все
fcp-batch-done = Усі завантаження завершено.
fcp-batch-failed = Не вдалося { $failed } з { $total } завантажень.
fcp-batch-canceled = Скасовано { $canceled } з { $total } завантажень.
fcp-dl-installing = Встановлення…
fcp-install-progress-label = Перебіг встановлення { $name }
fcp-install-done = Встановлено ✓
fcp-install-canceled = Встановлення скасовано.
fcp-install-failed = Встановлення не вдалося.
fcp-install-failed-not-downloaded = Спершу завантажте застосунок, щоб установити його.
fcp-install-failed-not-verified = Цей інсталятор не перевірений для тихого встановлення — його не було запущено.
fcp-install-failed-verify = Перевірка не пройдена — інсталятор відхилено.
fcp-install-failed-unsupported = Інсталятор цього типу не можна тихо встановити на цьому комп’ютері.
fcp-install-failed-installer = Інсталятор повідомив про помилку.
fcp-install-failed-elevation = У правах адміністратора відмовлено.
fcp-install-failed-busy = Встановлення вже триває.
fcp-open-app = Відкрити
fcp-open-failed = Не вдалося відкрити { $name }.
fcp-batch-installing = Встановлення… встановлено { $done } з { $total }
fcp-batch-installed = Усі застосунки встановлено.
fcp-batch-install-failed = Не вдалося { $failed } з { $total } встановлень.
fcp-batch-install-canceled = Скасовано { $canceled } з { $total } встановлень.
fcp-modal-close = Закрити
