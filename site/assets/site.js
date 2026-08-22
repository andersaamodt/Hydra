(function () {
  'use strict';

  var focus = document.querySelector('[data-download-focus]');
  if (!focus) return;

  var RELEASES_URL = 'https://github.com/andersaamodt/Hydra/releases';
  var SOURCE_URL = 'https://github.com/andersaamodt/Hydra#build-and-test';
  var RELEASE_API = 'https://api.github.com/repos/andersaamodt/Hydra/releases/latest';
  var platforms = {
    macos: { name: 'macOS', mark: '⌘', patterns: [/\.dmg$/i, /(?:mac|darwin|osx).*\.zip$/i] },
    windows: { name: 'Windows', mark: '', patterns: [/\.exe$/i, /\.msi$/i, /(?:win).*\.zip$/i] },
    linux: { name: 'Linux', mark: '$', patterns: [/\.appimage$/i, /\.deb$/i, /\.rpm$/i, /(?:linux).*\.tar\.gz$/i] }
  };

  function detectPlatform() {
    var reported = '';
    if (navigator.userAgentData && navigator.userAgentData.platform) reported = navigator.userAgentData.platform;
    reported += ' ' + (navigator.platform || '') + ' ' + (navigator.userAgent || '');
    if (/android|iphone|ipad/i.test(reported)) return '';
    if (/mac/i.test(reported)) return 'macos';
    if (/win/i.test(reported)) return 'windows';
    if (/linux|x11/i.test(reported)) return 'linux';
    return '';
  }

  function setMark(element, platform) {
    var details = platforms[platform];
    element.className = 'platform-mark large ' + platform;
    element.replaceChildren();
    if (platform === 'windows') {
      element.append(document.createElement('span'), document.createElement('span'), document.createElement('span'), document.createElement('span'));
    } else {
      element.textContent = details.mark;
    }
  }

  function chooseAsset(assets, platform) {
    var patterns = platforms[platform].patterns;
    var cleanAssets = assets.filter(function (asset) {
      return !/checksum|sha256|signature|\.sig$/i.test(asset.name);
    });
    var patternIndex;
    var match;
    for (patternIndex = 0; patternIndex < patterns.length; patternIndex += 1) {
      match = cleanAssets.find(function (asset) { return patterns[patternIndex].test(asset.name); });
      if (match) return match;
    }
    return null;
  }

  function formatKind(filename) {
    if (/\.dmg$/i.test(filename)) return 'DMG';
    if (/\.exe$/i.test(filename)) return 'Installer';
    if (/\.msi$/i.test(filename)) return 'MSI';
    if (/\.appimage$/i.test(filename)) return 'AppImage';
    if (/\.deb$/i.test(filename)) return 'DEB';
    if (/\.rpm$/i.test(filename)) return 'RPM';
    return 'Package';
  }

  function enableLink(link, href, label) {
    link.href = href;
    link.textContent = label;
    link.classList.remove('disabled');
    link.removeAttribute('aria-disabled');
  }

  function markUnavailable(link, label) {
    link.removeAttribute('href');
    link.textContent = label;
    link.classList.add('disabled');
    link.setAttribute('aria-disabled', 'true');
  }

  function updatePlatformRows(release) {
    Object.keys(platforms).forEach(function (platform) {
      var row = document.querySelector('[data-platform-row="' + platform + '"]');
      var link = row.querySelector('[data-platform-download]');
      var status = row.querySelector('[data-platform-status]');
      var asset = chooseAsset(release.assets || [], platform);
      if (asset) {
        status.textContent = release.tag_name + ' · ' + formatKind(asset.name);
        enableLink(link, asset.browser_download_url, 'Download');
      } else {
        status.textContent = 'No published build yet';
        markUnavailable(link, 'Not available');
      }
    });
  }

  function showPrimary(platform, release) {
    var title = focus.querySelector('[data-primary-title]');
    var copy = focus.querySelector('[data-primary-copy]');
    var link = focus.querySelector('[data-primary-download]');
    var mark = focus.querySelector('[data-primary-mark]');
    var asset;

    if (!platform) {
      mark.className = 'platform-mark large';
      mark.textContent = '?';
      title.textContent = 'Choose a system';
      copy.textContent = 'Pick a download below.';
      enableLink(link, RELEASES_URL, 'See all releases');
      return;
    }

    setMark(mark, platform);
    asset = chooseAsset(release.assets || [], platform);
    if (asset) {
      title.textContent = 'Download for ' + platforms[platform].name;
      copy.textContent = release.tag_name + ' · ' + formatKind(asset.name);
      enableLink(link, asset.browser_download_url, 'Download Hydra');
    } else {
      title.textContent = 'No ' + platforms[platform].name + ' build yet';
      copy.textContent = "There isn't a " + platforms[platform].name + ' download in this release.';
      enableLink(link, RELEASES_URL, 'See all releases');
    }
  }

  function showNoRelease(platform) {
    var title = focus.querySelector('[data-primary-title]');
    var copy = focus.querySelector('[data-primary-copy]');
    var link = focus.querySelector('[data-primary-download]');
    var mark = focus.querySelector('[data-primary-mark]');

    if (platform) {
      setMark(mark, platform);
    } else {
      mark.className = 'platform-mark large';
      mark.textContent = '?';
    }
    title.textContent = 'Downloads are not ready yet';
    copy.textContent = 'Hydra can be built from source in the meantime.';
    enableLink(link, SOURCE_URL, 'Build from source');
    Object.keys(platforms).forEach(function (key) {
      var row = document.querySelector('[data-platform-row="' + key + '"]');
      row.querySelector('[data-platform-status]').textContent = 'No public release yet';
      markUnavailable(row.querySelector('[data-platform-download]'), 'Not available');
    });
  }

  function showCheckFailure(platform) {
    var title = focus.querySelector('[data-primary-title]');
    var copy = focus.querySelector('[data-primary-copy]');
    var link = focus.querySelector('[data-primary-download]');
    var mark = focus.querySelector('[data-primary-mark]');
    if (platform) {
      setMark(mark, platform);
    } else {
      mark.className = 'platform-mark large';
      mark.textContent = '?';
    }
    title.textContent = 'Latest release';
    copy.textContent = 'Hydra could not check for downloads right now.';
    enableLink(link, RELEASES_URL, 'See all releases');
  }

  var detected = detectPlatform();
  if (detected) {
    var detectedRow = document.querySelector('[data-platform-row="' + detected + '"]');
    if (detectedRow) detectedRow.hidden = true;
  }

  fetch(RELEASE_API, { headers: { Accept: 'application/vnd.github+json' } })
    .then(function (response) {
      if (response.status === 404) return null;
      if (!response.ok) throw new Error('release check failed');
      return response.json();
    })
    .then(function (release) {
      if (!release) {
        showNoRelease(detected);
        return;
      }
      updatePlatformRows(release);
      showPrimary(detected, release);
    })
    .catch(function () {
      showCheckFailure(detected);
    })
    .finally(function () {
      focus.removeAttribute('aria-busy');
    });
}());
