/* Kashot landing page — interactive layer.
 *
 *  - boot screen fade
 *  - cursor-tracked spotlight
 *  - parallax grid-floor offset (scroll)
 *  - scroll-scrubbed scale on the hero screenshot frame
 *  - sticky scrollytelling step-tracker for #showcase
 *  - reveal-on-scroll (IntersectionObserver, fire-once)
 *  - counter tick-up on first reveal
 *  - smooth in-page anchors
 *  - GitHub release fetch → updates download URL + version label
 *
 * No frameworks. One closure. One requestAnimationFrame loop.
 */

(function () {
  'use strict';

  const root  = document.documentElement;
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const coarsePtr    = window.matchMedia('(pointer: coarse)').matches;

  root.setAttribute('data-theme', 'tron');

  // ── Boot fade ─────────────────────────────────────────────────────────
  window.addEventListener('load', () => {
    setTimeout(() => document.body.classList.add('booted'), 550);
  });

  // ── Smooth scroll for in-page anchors ─────────────────────────────────
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

  // ── Cursor spotlight ──────────────────────────────────────────────────
  const glow = document.querySelector('.cursor-glow');
  if (glow && !coarsePtr && !reduceMotion) {
    let tx = -200, ty = -200, x = -200, y = -200, raf = 0;
    document.addEventListener('mousemove', (e) => {
      tx = e.clientX; ty = e.clientY;
      if (!raf) raf = requestAnimationFrame(tickGlow);
    }, { passive: true });
    function tickGlow() {
      // ease toward target so the spotlight has a little drag
      x += (tx - x) * 0.18;
      y += (ty - y) * 0.18;
      glow.style.setProperty('--x', x + 'px');
      glow.style.setProperty('--y', y + 'px');
      if (Math.abs(tx - x) > 0.5 || Math.abs(ty - y) > 0.5) {
        raf = requestAnimationFrame(tickGlow);
      } else {
        raf = 0;
      }
    }
  }

  // ── Scroll-driven layer (rAF-throttled) ───────────────────────────────
  const gridFloor    = document.querySelector('.grid-floor');
  const scrubScale   = document.querySelector('[data-scrub-scale]');
  const scrubFade    = document.querySelector('[data-scrub-fade]');
  // `.showcase-shell` is the grid container; data-step now lives there
  // (previously on `.showcase-stage` which was the all-sticky parent we
  // removed when splitting the layout so only the left frame stays pinned).
  const showcaseStg  = document.querySelector('.showcase-shell');
  const showcaseSteps= Array.from(document.querySelectorAll('.showcase-step'));

  let pending = false;

  function onScroll() {
    if (pending) return;
    pending = true;
    requestAnimationFrame(() => {
      const y = window.scrollY || window.pageYOffset || 0;

      // 1. parallax grid floor (slow drift independent of CSS animation)
      if (gridFloor && !reduceMotion) {
        gridFloor.style.setProperty('--gridY', (y * 0.15) + 'px');
      }

      // 2. hero subtitle / CTAs fade as the hero leaves the viewport
      if (scrubFade) {
        const h = scrubFade.offsetHeight || 1;
        const p = Math.min(1, Math.max(0, y / (h * 0.9)));
        scrubFade.style.setProperty('opacity', String(1 - p * 0.85));
      }

      // 3. screenshot scrubbed scale — grows as you scroll into it
      if (scrubScale) {
        const r = scrubScale.getBoundingClientRect();
        const vh = window.innerHeight;
        // progress: 0 when section just enters from below, 1 when it's centered
        let p = 1 - (r.top + r.height * 0.4) / vh;
        p = Math.min(1, Math.max(0, p));
        const s = 0.94 + p * 0.06;     // 0.94 → 1.00
        const t = (1 - p) * 12;         // 12px → 0
        scrubScale.style.transform = `translateY(${t}px) scale(${s})`;
      }

      pending = false;
    });
  }

  // ── Showcase step tracker ─────────────────────────────────────────────
  // Pick whichever step's bounding rect is closest to the viewport vertical
  // centre, and mirror that into both the step's `.active` class (drives the
  // green left rule + brightening) and into `data-step` on the shell (drives
  // the SVG selection/annotation reveals inside the sticky frame on the
  // left). IntersectionObserver tells us *when* to recompute; the actual
  // pick uses geometric distance so it works regardless of step heights.
  if (showcaseStg && showcaseSteps.length) {
    const pickActive = () => {
      const mid = window.innerHeight / 2;
      let bestIdx = 0, bestDist = Infinity;
      showcaseSteps.forEach((el, i) => {
        const r = el.getBoundingClientRect();
        const c = r.top + r.height / 2;
        const d = Math.abs(c - mid);
        if (d < bestDist) { bestDist = d; bestIdx = i; }
      });
      const step = bestIdx + 1;
      // No early-exit on data-step match — the HTML pre-sets `data-step="1"`
      // for the initial paint, so an early-exit would skip the very first
      // `.active` class application when the page lands on step 1.
      showcaseStg.dataset.step = String(step);
      showcaseSteps.forEach((el, i) => {
        el.classList.toggle('active', i === bestIdx);
      });
    };
    if ('IntersectionObserver' in window) {
      // We don't actually use the IO entries — we just use IO as a
      // cheap "viewport activity" signal so we recompute on scroll without
      // adding another scroll listener.
      const io = new IntersectionObserver(pickActive, {
        rootMargin: '-30% 0px -30% 0px',
        threshold: [0, 0.5, 1],
      });
      showcaseSteps.forEach(el => io.observe(el));
    }
    window.addEventListener('scroll', pickActive, { passive: true });
    window.addEventListener('resize', pickActive, { passive: true });
    pickActive();
  }

  window.addEventListener('scroll', onScroll, { passive: true });
  window.addEventListener('resize', onScroll, { passive: true });
  onScroll();

  // ── Reveal on scroll ──────────────────────────────────────────────────
  const revealEls = document.querySelectorAll('.reveal');
  if ('IntersectionObserver' in window && !reduceMotion) {
    const io = new IntersectionObserver((entries) => {
      entries.forEach(e => {
        if (e.isIntersecting) {
          e.target.classList.add('in');
          io.unobserve(e.target);
        }
      });
    }, { rootMargin: '-8% 0px -8% 0px', threshold: 0.05 });
    revealEls.forEach(el => io.observe(el));
  } else {
    revealEls.forEach(el => el.classList.add('in'));
  }

  // ── Counter tick-up ───────────────────────────────────────────────────
  const counters = document.querySelectorAll('.counter');
  if ('IntersectionObserver' in window && !reduceMotion) {
    const cio = new IntersectionObserver((entries) => {
      entries.forEach(e => {
        if (e.isIntersecting) {
          tickCounter(e.target);
          cio.unobserve(e.target);
        }
      });
    }, { threshold: 0.4 });
    counters.forEach(el => cio.observe(el));
  } else {
    counters.forEach(el => { el.textContent = el.dataset.target || el.textContent; });
  }

  function tickCounter(el) {
    const target = parseInt(el.dataset.target, 10) || 0;
    const dur = 900;
    const start = performance.now();
    function step(now) {
      const t = Math.min(1, (now - start) / dur);
      // easeOutCubic
      const e = 1 - Math.pow(1 - t, 3);
      el.textContent = String(Math.round(target * e));
      if (t < 1) requestAnimationFrame(step);
      else el.textContent = String(target);
    }
    requestAnimationFrame(step);
  }

  // ── OS detection ──────────────────────────────────────────────────────
  // Picks Windows / macOS / Linux for the hero download button + sub-label
  // and highlights the matching card in the acquire grid. `navigator.platform`
  // is deprecated but every browser still implements it and on macOS / Linux
  // it is far more reliable than parsing userAgent. We treat iOS / Android as
  // "other" — the desktop app isn't a meaningful download there, so we leave
  // the default (Windows MSI) which mirrors current behavior on mobile.
  function detectOS() {
    const ua  = (navigator.userAgent || '').toLowerCase();
    const plt = (navigator.platform  || '').toLowerCase();
    if (/iphone|ipad|ipod|android/.test(ua)) return 'other';
    if (/mac|darwin/.test(plt) || /mac os x|macintosh/.test(ua)) return 'macos';
    if (/win/.test(plt) || /windows/.test(ua))                    return 'windows';
    if (/linux|x11|bsd/.test(plt) || /linux|x11|cros|ubuntu|fedora|arch|debian/.test(ua)) return 'linux';
    return 'other';
  }

  const detectedOS = detectOS();

  // Per-OS asset selectors and label suffixes. Each picker walks the release
  // asset list in priority order; the first match wins. Fallback URLs match
  // the static `href` attribute already on the buttons in index.html so the
  // page degrades cleanly if the GitHub API call fails.
  const OS_CONFIG = {
    windows: {
      label:    'WINDOWS · MSI',
      pickers:  [/\.msi$/i, /\.exe$/i, /-portable\.zip$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/Kashot.msi',
      cardId:   'WINDOWS',
    },
    macos: {
      label:    'MACOS · APPLE SILICON',
      pickers:  [/macos.*arm64/i, /macos.*aarch64/i, /macos.*x64/i, /macos/i, /\.dmg$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/Kashot-macos-arm64',
      cardId:   'MACOS',
    },
    linux: {
      label:    'LINUX · TAR.GZ',
      pickers:  [/linux.*x86_64\.tar\.gz$/i, /linux.*\.tar\.gz$/i, /\.AppImage$/i, /linux/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/kashot-linux-x86_64.tar.gz',
      cardId:   'LINUX',
    },
    other: {
      label:    'CHOOSE BUILD',
      pickers:  [/\.msi$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest',
      cardId:   null,
    },
  };

  const cfg     = OS_CONFIG[detectedOS] || OS_CONFIG.other;
  const heroBtn = document.getElementById('download-btn');
  const sub     = document.getElementById('download-sub');

  // Apply per-OS fallback immediately so the button works even if the
  // GitHub API fetch below is slow / fails / is rate-limited.
  if (heroBtn) heroBtn.href = cfg.fallback;
  if (sub)     sub.textContent = `v0.1 · ${cfg.label}`;

  // Spotlight the matching card in the acquire grid so the user lands on
  // their OS's section visually. We clear the existing `.dl-card-active`
  // class (statically set on the Windows card) before applying ours, so
  // detected non-Windows OSes get exclusive highlight.
  if (cfg.cardId) {
    document.querySelectorAll('.dl-card-active').forEach(c => c.classList.remove('dl-card-active'));
    document.querySelectorAll('.dl-card').forEach(card => {
      const h3 = card.querySelector('h3');
      if (h3 && h3.textContent.trim().toUpperCase() === cfg.cardId) {
        card.classList.add('dl-card-active');
      }
    });
  }

  // Highlight the matching platform card in the hero platforms strip. The
  // strip has three cards keyed by `data-os="windows|linux|macos"`; we
  // flip `.is-detected` onto the one matching `detectedOS` so the user
  // can see "yes, the site recognized me" at a glance. Mobile / unknown
  // OSes leave the strip in its neutral state.
  document.querySelectorAll('.platform.is-detected').forEach(p => p.classList.remove('is-detected'));
  if (['windows', 'linux', 'macos'].includes(detectedOS)) {
    const match = document.querySelector(`.platform[data-os="${detectedOS}"]`);
    if (match) match.classList.add('is-detected');
  }

  // ── GitHub release fetch ──────────────────────────────────────────────
  // Look up the real asset URL + version tag from the latest release. If
  // the API call fails (rate limit / offline), the static fallback wired
  // above keeps working as long as a release exists.
  const REPO = 'singhpratech/kashot';

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { 'Accept': 'application/vnd.github+json' }
  })
    .then(r => r.ok ? r.json() : Promise.reject(r.status))
    .then(release => {
      const assets = release.assets || [];
      const tag    = release.tag_name || release.name || 'v0.1';

      // Walk pickers in priority order; first asset matching any picker wins.
      let chosen = null;
      for (const re of cfg.pickers) {
        chosen = assets.find(a => re.test(a.name));
        if (chosen) break;
      }

      if (sub) sub.textContent = `${tag} · ${cfg.label}`;

      if (chosen && heroBtn) {
        heroBtn.href = chosen.browser_download_url;
      }

      // Keep the legacy .msi-hardcoded links in the acquire grid working
      // by rewriting any that point at the static fallback URL.
      const msi = assets.find(a => /\.msi$/i.test(a.name));
      if (msi) {
        document.querySelectorAll('a[href*="releases/latest/download/Kashot.msi"]')
          .forEach(a => { a.href = msi.browser_download_url; });
      }
    })
    .catch(() => { /* keep static fallback */ });

})();
