# Freally Central panel — fcp-* strings (ru)
# Loaded into the HOST app's Fluent bundles alongside its own catalogs.
# Keys must stay in sync across all locales (npm run i18n:lint).

fcp-filter-type = Тип
fcp-filter-all = Все
fcp-available = Доступные
fcp-coming-soon = Скоро
fcp-install-all = Скачать и установить все
fcp-install-all-hint = Скачивает и тихо устанавливает все доступные приложения для этого компьютера.
fcp-install-all-unsupported = Для установки нужно настольное приложение Freally Central.
fcp-install-all-none = Пока нет приложений для скачивания.
fcp-detail-back = Назад
fcp-detail-features = Возможности
fcp-detail-started = Начато { $date }
fcp-detail-visit-site = Открыть сайт
fcp-detail-no-features = Подробности о возможностях скоро.
fcp-detail-coming-soon-note = Это приложение пока недоступно для загрузки.
fcp-status-offline = Офлайн — показан встроенный каталог.
fcp-grid-empty = Ни одно приложение не соответствует этому фильтру.
fcp-whats-new-title = Что нового
fcp-downloads-count =
    { $count ->
        [one] { $count } загрузка
        [few] { $count } загрузки
        [many] { $count } загрузок
       *[other] { $count } загрузки
    }
fcp-downloads-total = Всего
fcp-downloads-brandwide = Приложения Freally загружены { $count } раз
fcp-downloads-unavailable = Число загрузок сейчас недоступно.
fcp-detail-released = Выпущено { $date }
fcp-detail-downloads = Загрузки
fcp-detail-whats-new = Что нового
fcp-refresh = Обновить
fcp-changelog-heading = Что нового в { $name }
fcp-changelog-version = Версия { $version } · { $date }
fcp-changelog-empty = Пока нет примечаний к выпуску.
fcp-changelog-view-full = Посмотреть полный список изменений
fcp-changelog-view-release = Открыть на GitHub
fcp-card-download = Скачать
fcp-tagline-freally-capture = Записывайте и стримьте как в студии — одно приложение.
fcp-tagline-freally-vault = Ваши пароли, passkey и секреты — зашифрованы, local-first, ваши.
fcp-tagline-freally-player = Играет всё. Красиво. Без рекламы и шпионов.
fcp-tagline-freally-av = Freally Anti-Virus — скоро.
fcp-tagline-freally-studio = Музыкальная студия с ИИ — скоро.
fcp-tagline-freally-sourcerer = Будущее приложение Freally.
fcp-tagline-freally-file-manager = Кроссплатформенный файловый менеджер — скоро.
fcp-desc-freally-capture = Локальная кроссплатформенная студия стриминга и записи (уровня OBS): GPU-композитор сцен в реальном времени, многодорожечная запись и мультистрим на все платформы сразу — целиком на вашем компьютере. Без аккаунтов, без телеметрии, без облака.
fcp-desc-freally-vault = Локальный менеджер паролей, passkey и секретов с нулевым разглашением и сквозным шифрованием. Совместим с KeePass, безопасное для памяти ядро на Rust, полностью бесплатно — ваше хранилище никогда не покидает ваш компьютер.
fcp-desc-freally-player = Локальный кроссплатформенный медиаплеер, который воспроизводит практически всё с аппаратным ускорением — современный интерфейс, настоящая медиатека и стриминг, без рекламы и телеметрии.
fcp-desc-freally-av = Локальный антивирус Freally для Windows, macOS и Linux. В разработке — пока недоступен для загрузки.
fcp-desc-freally-studio = Полнофункциональная DAW с ИИ-генерацией паттернов в 35+ жанрах, микшером в стиле FL, таймлайном аранжировки и редактором аудиоклипов. Предварительная версия — пока недоступна для загрузки.
fcp-desc-freally-sourcerer = Это приложение Freally в разработке. Пока недоступно для загрузки.
fcp-desc-freally-file-manager = Это приложение Freally в разработке. Пока недоступно для загрузки.
fcp-feat-freally-capture-1 = Собственный GPU-композитор сцен (wgpu) с фильтрами и переходами для каждого источника
fcp-feat-freally-capture-2 = Многодорожечная запись (аппаратные кодировщики + собственный кодек без потерь)
fcp-feat-freally-capture-3 = Мультистрим на Twitch/YouTube/Kick одновременно (RTMP/SRT/WHIP)
fcp-feat-freally-capture-4 = Виртуальная камера, буфер повторов, downstream-кееры и 3D-трансформации
fcp-feat-freally-capture-5 = Интерфейс на 18 языках
fcp-status-installed = Установлено
fcp-status-update = Доступно обновление
fcp-status-not-installed = Не установлено
fcp-action-install = Установить
fcp-action-update = Обновить
fcp-action-redownload = Скачать заново
fcp-dl-downloading = Скачивание…
fcp-dl-verifying = Проверка…
fcp-dl-done-verified = Скачано и проверено
fcp-dl-done-size-only = Скачано (размер сверен)
fcp-dl-failed = Не удалось скачать.
fcp-dl-failed-network = Не удалось скачать — ошибка сети.
fcp-dl-failed-verify = Проверка не пройдена — файл удалён.
fcp-dl-failed-busy = Для этого приложения уже идёт скачивание.
fcp-dl-canceled = Скачивание отменено.
fcp-dl-cancel = Отмена
fcp-dl-retry = Повторить
fcp-dl-progress-label = Ход скачивания { $name }
fcp-dl-show-in-folder = Показать в папке
fcp-batch-progress = Скачано { $done } из { $total }
fcp-batch-cancel-all = Отменить всё
fcp-batch-done = Все загрузки завершены.
fcp-batch-failed = Не удалось { $failed } из { $total } загрузок.
fcp-batch-canceled = Отменено { $canceled } из { $total } загрузок.
fcp-dl-installing = Установка…
fcp-install-progress-label = Ход установки { $name }
fcp-install-done = Установлено ✓
fcp-install-canceled = Установка отменена.
fcp-install-failed = Установка не удалась.
fcp-install-failed-not-downloaded = Сначала скачайте приложение, чтобы установить его.
fcp-install-failed-not-verified = Этот установщик не проверен для тихой установки — он не был запущен.
fcp-install-failed-verify = Проверка не пройдена — установщик отклонён.
fcp-install-failed-unsupported = Установщик этого типа нельзя тихо установить на этом компьютере.
fcp-install-failed-installer = Установщик сообщил об ошибке.
fcp-install-failed-elevation = В правах администратора отказано.
fcp-install-failed-busy = Установка уже выполняется.
fcp-open-app = Открыть
fcp-open-failed = Не удалось открыть { $name }.
fcp-batch-installing = Установка… установлено { $done } из { $total }
fcp-batch-installed = Все приложения установлены.
fcp-batch-install-failed = Не удалось { $failed } из { $total } установок.
fcp-batch-install-canceled = Отменено { $canceled } из { $total } установок.
fcp-modal-close = Закрыть
