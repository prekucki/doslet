const I18N = {
    en: {
        released: 'Released',
        publisher: 'Publisher',
        developer: 'Developer',
        genre: 'Genre',
        gameplay: 'Gameplay',
        launch: 'Play game',
        loadingEmulator: 'Loading emulator…',
        ready: 'Ready to play',
        loadingAssets: 'Loading game, profile and ROMs…',
        failed: 'Failed to launch: ',
        insecureContext: 'Open this page over HTTPS or http://localhost on the server machine. Plain HTTP over a LAN IP does not support persistent saves or audio.',
    },
    pl: {
        released: 'Wydanie',
        publisher: 'Wydawca',
        developer: 'Producent',
        genre: 'Gatunek',
        gameplay: 'Rozgrywka',
        launch: 'Uruchom grę',
        loadingEmulator: 'Ładowanie emulatora…',
        ready: 'Gotowe do uruchomienia',
        loadingAssets: 'Wczytywanie gry, profilu i ROM-ów…',
        failed: 'Nie udało się uruchomić: ',
        insecureContext: 'Otwórz stronę przez HTTPS albo http://localhost na komputerze z serwerem. HTTP pod adresem IP w sieci LAN nie obsługuje zapisów i audio.',
    },
};

function detectLocale() {
    const candidates = navigator.languages?.length ? navigator.languages : [navigator.language || 'en'];
    for (const tag of candidates) {
        const primary = String(tag).trim().split(/[-_]/)[0].toLowerCase();
        if (primary in I18N) {
            return I18N[primary];
        }
    }
    return I18N.en;
}

const strings = detectLocale();
for (const element of document.querySelectorAll('#doslet-bundle-info dt[data-i18n]')) {
    const key = element.getAttribute('data-i18n');
    if (key && key in strings) {
        element.textContent = strings[key];
    }
}

const button = document.getElementById('doslet-bundle-launch');
const status = document.getElementById('doslet-bundle-status');
if (button && status) {
    button.textContent = strings.launch;
    status.textContent = strings.loadingEmulator;
    if (!window.isSecureContext) {
        button.disabled = true;
        status.textContent = strings.insecureContext;
    }
    const ready = setInterval(() => {
        if (window.dosletBundleBridge) {
            clearInterval(ready);
            if (window.isSecureContext) {
                button.disabled = false;
                status.textContent = strings.ready;
            }
        }
    }, 100);
    button.addEventListener('click', () => {
        button.disabled = true;
        status.textContent = strings.loadingAssets;
        window.dispatchEvent(new Event('doslet-launch-bundle'));
    });
    window.addEventListener('doslet-bundle-status', ({detail}) => {
        if (detail.ready) {
            document.getElementById('doslet-bundle-cover').remove();
            document.querySelector('canvas')?.focus();
        }
        else {
            button.disabled = false;
            status.textContent = `${strings.failed}${detail.error}`;
        }
    });
}
