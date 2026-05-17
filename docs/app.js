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
  //
  // Same Rust binary on all three platforms as of v0.2.0 — the picker order
  // prefers the new canonical artifacts, with the legacy C# MSI/EXE/ZIP kept
  // as fallback for the Windows acquire-card "INSTALLER" buttons.
  const OS_CONFIG = {
    windows: {
      label:    'WINDOWS · ZIP',
      pickers:  [/windows.*x86_64\.zip$/i, /\.msi$/i, /\.exe$/i, /-portable\.zip$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest/download/kashot-windows-x86_64.zip',
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
      pickers:  [/windows.*x86_64\.zip$/i, /\.msi$/i],
      fallback: 'https://github.com/singhpratech/kashot/releases/latest',
      cardId:   null,
    },
  };

  const heroBtn = document.getElementById('download-btn');
  const sub     = document.getElementById('download-sub');

  // Latest GitHub Release payload cached here once the fetch returns, so a
  // user clicking another OS gets the real asset URL too (not just the
  // static fallback). Until the fetch completes we fall back to defaults.
  let latestRelease = null;
  let activeOS      = null;

  function applyOS(os) {
    const cfg = OS_CONFIG[os] || OS_CONFIG.other;
    activeOS  = os;

    const tag = (latestRelease && (latestRelease.tag_name || latestRelease.name)) || 'v0.2';
    let href  = cfg.fallback;

    if (latestRelease && latestRelease.assets) {
      for (const re of cfg.pickers) {
        const hit = latestRelease.assets.find(a => re.test(a.name));
        if (hit) { href = hit.browser_download_url; break; }
      }
    }

    if (heroBtn) heroBtn.href = href;
    if (sub)     sub.textContent = `${tag} · ${cfg.label}`;

    // Hero platforms strip — mark the active card.
    document.querySelectorAll('.platform.is-detected').forEach(p => p.classList.remove('is-detected'));
    if (['windows', 'linux', 'macos'].includes(os)) {
      const match = document.querySelector(`.platform[data-os="${os}"]`);
      if (match) match.classList.add('is-detected');
    }

    // Acquire grid — spotlight the matching dl-card.
    if (cfg.cardId) {
      document.querySelectorAll('.dl-card-active').forEach(c => c.classList.remove('dl-card-active'));
      document.querySelectorAll('.dl-card').forEach(card => {
        const h3 = card.querySelector('h3');
        if (h3 && h3.textContent.trim().toUpperCase() === cfg.cardId) {
          card.classList.add('dl-card-active');
        }
      });
    }
  }

  // First paint uses auto-detect; the user can override by clicking any
  // platform card in the hero strip or any dl-card in the acquire grid.
  applyOS(detectedOS);

  function wireClick(el, os) {
    if (!el) return;
    el.style.cursor = 'pointer';
    el.setAttribute('role', 'button');
    el.setAttribute('tabindex', '0');
    el.setAttribute('aria-label', `Show ${os} install instructions`);
    const handler = (ev) => {
      ev.preventDefault();
      applyOS(os);
      // Scroll the acquire grid into view so the user sees the swap.
      const acquire = document.getElementById('acquire');
      if (acquire) acquire.scrollIntoView({ behavior: 'smooth', block: 'start' });
    };
    el.addEventListener('click', handler);
    el.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' || ev.key === ' ') handler(ev);
    });
  }

  // Hero strip — every platform card overrides the detected OS.
  document.querySelectorAll('.platform[data-os]').forEach(p => {
    const os = p.getAttribute('data-os');
    if (['windows', 'linux', 'macos'].includes(os)) wireClick(p, os);
  });

  // Acquire grid — clicking the dl-card header (but not the buttons inside)
  // also re-targets the active OS. We attach to the <header> so the actual
  // download buttons keep their native click behaviour.
  document.querySelectorAll('.dl-card').forEach(card => {
    const h3 = card.querySelector('h3');
    if (!h3) return;
    const label = h3.textContent.trim().toLowerCase();
    if (['windows', 'linux', 'macos'].includes(label)) {
      wireClick(card.querySelector('header') || card, label);
    }
  });

  // ── GitHub release fetch ──────────────────────────────────────────────
  // Look up the real asset URL + version tag from the latest release. If
  // the API call fails (rate limit / offline), the static fallbacks above
  // keep working as long as a release exists.
  const REPO = 'singhpratech/kashot';

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
    headers: { 'Accept': 'application/vnd.github+json' }
  })
    .then(r => r.ok ? r.json() : Promise.reject(r.status))
    .then(release => {
      latestRelease = release;
      const tag = release.tag_name || release.name || 'v0.2';

      // Re-apply the active OS so the hero button + label update with the
      // real release URL + tag, not just the static fallback.
      applyOS(activeOS || detectedOS);

      // Bump every static "v0.1 / v0.2" version label on the page to the
      // real tag so the topbar and brand tag don't lie.
      document.querySelectorAll('[data-version]').forEach(el => {
        el.textContent = tag;
      });

      // Rewrite all `releases/latest/download/*` asset links to the actual
      // tagged URLs, so download links keep working even after future
      // releases shift "latest" to a tag that doesn't include this asset.
      const assets = release.assets || [];
      const byName = new Map(assets.map(a => [a.name, a.browser_download_url]));
      document.querySelectorAll('a[href*="releases/latest/download/"]').forEach(a => {
        const m = a.href.match(/releases\/latest\/download\/([^/?#]+)/);
        if (m && byName.has(m[1])) a.href = byName.get(m[1]);
      });
    })
    .catch(() => { /* keep static fallbacks */ });

  // ── Copy buttons on .dl-pipe ──────────────────────────────────────────
  // Each install-command code block has a COPY chip in the top-right.
  // navigator.clipboard.writeText is the modern path; falls back to a
  // Selection + execCommand('copy') for older browsers / non-https.
  document.querySelectorAll('.dl-pipe-copy').forEach(btn => {
    btn.addEventListener('click', async () => {
      const wrap = btn.closest('.dl-pipe-wrap');
      const pre  = wrap && wrap.querySelector('.dl-pipe');
      if (!pre) return;
      const text = pre.textContent.trim();
      let ok = false;
      try {
        await navigator.clipboard.writeText(text);
        ok = true;
      } catch (_) {
        try {
          const range = document.createRange();
          range.selectNode(pre);
          const sel = window.getSelection();
          sel.removeAllRanges();
          sel.addRange(range);
          ok = document.execCommand('copy');
          sel.removeAllRanges();
        } catch (__) { /* swallow */ }
      }
      if (!ok) return;
      btn.textContent = 'COPIED';
      btn.classList.add('is-copied');
      clearTimeout(btn._copyTimer);
      btn._copyTimer = setTimeout(() => {
        btn.textContent = 'COPY';
        btn.classList.remove('is-copied');
      }, 1400);
    });
  });

  // ── Ambient site music (procedurally generated, original) ─────────────
  // 100% synthesized live in the browser via Web Audio. No samples, no
  // external audio files, no third-party melodies. Four saw voices form
  // a chord pad that walks an A-minor / F / C / G progression on an
  // 8-second-per-chord cycle. A separate "swell" gain modulates the
  // overall amplitude up and down on a slow sine to create an emotional
  // breath, and a high shimmer voice fades in/out for lift.
  //
  // Default ON. Browser autoplay policies block AudioContext until a
  // user gesture, so we arm the synth on the first click / scroll /
  // keypress / touch — instant once the user does anything. The topbar
  // toggle still lets you mute, and the preference persists.
  const audioBtn   = document.getElementById('audio-toggle');
  const audioState = document.getElementById('audio-state');
  let audioCtx     = null;
  let audioMaster  = null;
  let audioStarted = false;
  let chordOscs    = [];       // {osc, target} per voice
  let chordTimer   = null;

  // Four chords, four voices each (root / 5th / oct / 10th-ish). Hand-
  // picked so the bass voice walks A1 → F1 → C2 → G1 — descent then
  // lift — for an arc that climbs into the third chord and resolves.
  const CHORDS = [
    [55.00,  82.41, 110.00, 164.81], // Am  (A1, E2, A2, E3)
    [43.65,  65.41,  87.31, 174.61], // F   (F1, C2, F2, F3)
    [65.41,  98.00, 130.81, 196.00], // C   (C2, G2, C3, G3)
    [49.00,  73.42,  98.00, 146.83], // G   (G1, D2, G2, D3)
  ];
  const CHORD_SECS = 8;

  function startAudio() {
    if (audioStarted) return;
    const Ctor = window.AudioContext || window.webkitAudioContext;
    if (!Ctor) return;
    audioStarted = true;
    audioCtx = new Ctor();

    // Master chain: master -> destination
    audioMaster = audioCtx.createGain();
    audioMaster.gain.value = 0;
    audioMaster.connect(audioCtx.destination);

    // Swell gain — sits between the pad bus and master, modulated by a
    // slow sine LFO to breathe between intimate and uplifting.
    const swell = audioCtx.createGain();
    swell.gain.value = 0.7;
    swell.connect(audioMaster);

    const swellLfo = audioCtx.createOscillator();
    swellLfo.type = 'sine';
    swellLfo.frequency.value = 1 / 18; // 18-second emotional breath
    const swellDepth = audioCtx.createGain();
    swellDepth.gain.value = 0.25;       // swings 0.45 ↔ 0.95
    swellLfo.connect(swellDepth);
    swellDepth.connect(swell.gain);
    swellLfo.start();

    // Lowpass on the pad bus — slow filter motion adds texture beyond
    // just amplitude.
    const lpf = audioCtx.createBiquadFilter();
    lpf.type = 'lowpass';
    lpf.frequency.value = 600;
    lpf.Q.value = 2.5;
    lpf.connect(swell);

    const filtLfo = audioCtx.createOscillator();
    filtLfo.type = 'sine';
    filtLfo.frequency.value = 1 / 13; // out of phase with swell on purpose
    const filtDepth = audioCtx.createGain();
    filtDepth.gain.value = 360;
    filtLfo.connect(filtDepth);
    filtDepth.connect(lpf.frequency);
    filtLfo.start();

    const hpf = audioCtx.createBiquadFilter();
    hpf.type = 'highpass';
    hpf.frequency.value = 45;
    hpf.connect(lpf);

    // Four pad voices — two saws each, detuned for thickness.
    const initial = CHORDS[0];
    for (let i = 0; i < 4; i++) {
      const voiceGain = audioCtx.createGain();
      voiceGain.gain.value = 0.13 / Math.pow(i + 1, 0.55);
      voiceGain.connect(hpf);
      const pair = [];
      for (let j = 0; j < 2; j++) {
        const osc = audioCtx.createOscillator();
        osc.type = 'sawtooth';
        osc.frequency.value = initial[i];
        osc.detune.value = j === 0 ? -7 : +7;
        osc.connect(voiceGain);
        osc.start();
        pair.push(osc);
      }
      chordOscs.push(pair);
    }

    // Shimmer voice — sine an octave above the top pad note, comes in
    // on the lifting half of the swell to add emotional peak.
    const shimmerGain = audioCtx.createGain();
    shimmerGain.gain.value = 0;
    shimmerGain.connect(audioMaster);
    const shimmer = audioCtx.createOscillator();
    shimmer.type = 'sine';
    shimmer.frequency.value = initial[3] * 2;
    shimmer.connect(shimmerGain);
    shimmer.start();
    // Slow shimmer envelope: 0 → 0.06 → 0 every 22s, offset from swell.
    const shimmerLfo = audioCtx.createOscillator();
    shimmerLfo.type = 'sine';
    shimmerLfo.frequency.value = 1 / 22;
    const shimmerDepth = audioCtx.createGain();
    shimmerDepth.gain.value = 0.03;
    const shimmerBias = audioCtx.createConstantSource();
    shimmerBias.offset.value = 0.03;
    shimmerLfo.connect(shimmerDepth);
    shimmerDepth.connect(shimmerGain.gain);
    shimmerBias.connect(shimmerGain.gain);
    shimmerLfo.start();
    shimmerBias.start();

    // Chord scheduler — glide each voice to the next chord every
    // CHORD_SECS, smoothly so it feels like motion not steps.
    let chordIdx = 1;
    const scheduleNextChord = () => {
      const t = audioCtx.currentTime;
      const next = CHORDS[chordIdx % CHORDS.length];
      chordOscs.forEach((pair, vi) => {
        pair.forEach((osc) => {
          osc.frequency.cancelScheduledValues(t);
          osc.frequency.setValueAtTime(osc.frequency.value, t);
          osc.frequency.linearRampToValueAtTime(next[vi], t + 1.4);
        });
      });
      // Move shimmer to the top voice of the new chord, octave up.
      shimmer.frequency.cancelScheduledValues(t);
      shimmer.frequency.setValueAtTime(shimmer.frequency.value, t);
      shimmer.frequency.linearRampToValueAtTime(next[3] * 2, t + 1.4);
      chordIdx++;
    };
    chordTimer = setInterval(scheduleNextChord, CHORD_SECS * 1000);

    // Master fade-in.
    audioMaster.gain.cancelScheduledValues(audioCtx.currentTime);
    audioMaster.gain.setValueAtTime(0, audioCtx.currentTime);
    audioMaster.gain.linearRampToValueAtTime(0.24, audioCtx.currentTime + 3);
  }

  // 3 states: 'on' (audio is actually producing sound), 'armed' (we want
  // audio but the browser hasn't given us a user gesture yet so the
  // AudioContext is still asleep), 'off' (user has muted). The 'armed'
  // state is honest about what's happening — showing ON before first
  // gesture made it look broken when nothing came through the speakers.
  function setAudioUI(state) {
    const label = state === 'on'   ? 'ON'
                : state === 'armed' ? 'ARMED'
                                    : 'OFF';
    if (audioState) audioState.textContent = label;
    if (audioBtn) {
      audioBtn.setAttribute('aria-pressed', String(state === 'on'));
      audioBtn.setAttribute('data-state', state);
      audioBtn.title = state === 'armed'
        ? 'Audio is armed — click or scroll anywhere to start (browser autoplay policy)'
        : state === 'on'
          ? 'Mute ambient site audio'
          : 'Enable ambient site audio';
    }
  }

  function saveAudioPref(on) {
    try { localStorage.setItem('kashot.audio.v2', on ? '1' : '0'); } catch (_) {}
  }

  function muteAudio() {
    if (!audioStarted || !audioCtx) return;
    audioMaster.gain.cancelScheduledValues(audioCtx.currentTime);
    audioMaster.gain.linearRampToValueAtTime(0, audioCtx.currentTime + 0.4);
    setTimeout(() => { try { audioCtx.suspend(); } catch (_) {} }, 450);
  }

  function unmuteAudio() {
    if (!audioCtx) { startAudio(); return; }
    audioCtx.resume();
    audioMaster.gain.cancelScheduledValues(audioCtx.currentTime);
    audioMaster.gain.linearRampToValueAtTime(0.24, audioCtx.currentTime + 0.8);
  }

  if (audioBtn) {
    audioBtn.addEventListener('click', () => {
      if (!audioStarted) {
        startAudio();
        setAudioUI('on');
        saveAudioPref(true);
        return;
      }
      if (audioCtx.state === 'running') {
        muteAudio();
        setAudioUI('off');
        saveAudioPref(false);
      } else {
        unmuteAudio();
        setAudioUI('on');
        saveAudioPref(true);
      }
    });
  }

  // Default ON unless the user has explicitly muted on a previous visit.
  // Browsers require a user gesture before AudioContext can produce sound
  // so we arm the first gesture to autostart instantly. Until that gesture
  // we show 'ARMED' (honest) rather than 'ON' (a lie if no sound is out).
  let storedPref = null;
  try { storedPref = localStorage.getItem('kashot.audio.v2'); } catch (_) {}
  const wantAudio = storedPref === null ? true : storedPref === '1';
  setAudioUI(wantAudio ? 'armed' : 'off');

  if (wantAudio) {
    // Broad net of events that count as a "user activation" across browsers
    // (per the HTML spec): click / pointerup / pointerdown / keydown /
    // wheel / touchend. Some, like scroll on its own, are not a guaranteed
    // activation but we listen anyway as a fallback.
    const armEvents = [
      'pointerdown', 'pointerup', 'click', 'keydown',
      'touchstart', 'touchend', 'wheel', 'scroll',
    ];
    const arm = () => {
      armEvents.forEach((ev) =>
        window.removeEventListener(ev, arm, { capture: true })
      );
      if (!audioStarted) {
        startAudio();
        // startAudio() fades master gain up over 3s but flag UI as on
        // immediately so the user gets feedback the moment they interact.
        setAudioUI('on');
        saveAudioPref(true);
      }
    };
    armEvents.forEach((ev) =>
      window.addEventListener(ev, arm, { capture: true, passive: true })
    );
  }

})();
