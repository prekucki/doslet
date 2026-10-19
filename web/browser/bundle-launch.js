const button = document.getElementById('doslet-bundle-launch');
const status = document.getElementById('doslet-bundle-status');
if (button && status) {
    if (!window.isSecureContext) {
        button.disabled = true;
        status.textContent = 'Otwórz stronę przez HTTPS albo http://localhost na komputerze z serwerem. HTTP pod adresem IP w sieci LAN nie obsługuje zapisów i audio.';
    }
    const ready = setInterval(() => {
        if (window.dosletBundleBridge) {
            clearInterval(ready);
            if (window.isSecureContext) {
                button.disabled = false;
                status.textContent = 'Gotowe do uruchomienia';
            }
        }
    }, 100);
    button.addEventListener('click', () => {
        button.disabled = true;
        status.textContent = 'Wczytywanie gry, profilu i ROM-ów…';
        window.dispatchEvent(new Event('doslet-launch-bundle'));
    });
    window.addEventListener('doslet-bundle-status', ({detail}) => {
        if (detail.ready) {
            document.getElementById('doslet-bundle-cover').remove();
            document.querySelector('canvas')?.focus();
        }
        else {
            button.disabled = false;
            status.textContent = `Nie udało się uruchomić: ${detail.error}`;
        }
    });
}
