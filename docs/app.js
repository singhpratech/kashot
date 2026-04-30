/* Kashot landing page — small interactions only.
 * - theme toggle (persisted)
 * - latest release version, fetched lazily from GitHub
 * - graceful download fallback if MSI not yet attached to the latest release
 */

(function () {
  'use strict';

  // --- Theme ---------------------------------------------------------------

  const root = document.documentElement;
  const stored = localStorage.getItem('kashot-theme');
  const prefersLight = window.matchMedia('(prefers-color-scheme: light)').matches;
  const initial = stored || (prefersLight ? 'light' : 'dark');
  root.setAttribute('data-theme', initial);

  const toggle = document.querySelector('.theme-toggle');
  if (toggle) {
    toggle.addEventListener('click', () => {
      const next = root.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      localStorage.setItem('kashot-theme', next);
    });
  }

  // --- Smooth scroll for in-page anchors -----------------------------------

  document.querySelectorAll('a[href^="#"]').forEach(a => {
    a.addEventListener('click', (e) => {
      const id = a.getAttribute('href').slice(1);
      if (!id) return;
      const t = document.getElementById(id);
      if (!t) return;
      e.preventDefault();
      t.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  });

  // --- Latest release version + asset detection ----------------------------
  // Pulls the latest release info from GitHub, finds the Windows .msi asset,
  // wires its real download URL into every primary download button, and
  // updates the "latest release · 64-bit · MSI" subtitle with the version.
  // If the API call fails (rate limit, offline) the buttons keep their
  // hard-coded `/releases/latest/download/Kashot.msi` URL — which still works
  // as long as a release exists.

  const REPO = 'singhpratech/kashot';
  const sub = document.getElementById('download-sub');

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { 'Accept': 'application/vnd.github+json' }
  })
    .then(r => r.ok ? r.json() : Promise.reject(r.status))
    .then(release => {
      const msi = (release.assets || []).find(a => /\.msi$/i.test(a.name));
      const tag = release.tag_name || release.name || '';

      if (sub && tag) {
        sub.textContent = `${tag} · 64-bit · MSI`;
      }

      if (msi) {
        document.querySelectorAll('a[href*="releases/latest/download/Kashot.msi"]')
          .forEach(a => { a.href = msi.browser_download_url; });
      }
    })
    .catch(() => { /* keep static fallback */ });

})();
