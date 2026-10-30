/* Progressive enhancements for mdBook 0.4 and 0.5. Content works without JS. */
(() => {
  'use strict';
  const main = document.querySelector('main');
  if (!main || /(?:^|\/)print\.html$/.test(location.pathname)) return;
  const root = typeof path_to_root === 'string' ? path_to_root : './';
  const select = (name) => document.getElementById(`mdbook-${name}`) || document.getElementById(name);
  const make = (tag, className, text) => {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text) element.textContent = text;
    return element;
  };

  main.id = main.id || 'main-content';
  main.tabIndex = -1;
  const skip = make('a', 'rb-skip', 'Skip to content');
  skip.href = `#${main.id}`;
  document.body.prepend(skip);

  const sidebar = document.querySelector('.sidebar-scrollbox');
  if (sidebar) {
    const brand = make('a', 'rb-brand');
    brand.href = `${root}index.html`;
    brand.setAttribute('aria-label', 'Rustybin documentation home');
    const mark = make('span', 'rb-mark', 'r:');
    mark.setAttribute('aria-hidden', 'true');
    const name = make('span');
    name.append(make('strong', '', 'rustybin'), make('small', '', 'The field guide'));
    brand.append(mark, name);
    sidebar.prepend(brand);
  }

  const searchToggle = select('search-toggle');
  const searchbar = select('searchbar');
  if (searchToggle && searchbar) {
    searchbar.placeholder = 'Search tasks, endpoints, settings...';
    searchbar.setAttribute('aria-label', 'Search documentation');
    const button = make('button', 'rb-search-button');
    button.type = 'button';
    button.setAttribute('aria-label', 'Search documentation');
    button.append(make('span', '', 'Search docs'), make('kbd', '', '/'));
    searchToggle.after(button);
    // Keep the built-in control for mdBook's keyboard and focus handling.
    searchToggle.style.display = 'none';
    const openSearch = () => {
      if (searchToggle.getAttribute('aria-expanded') !== 'true') searchToggle.click();
      searchbar.focus();
    };
    button.addEventListener('click', openSearch);
    document.addEventListener('keydown', (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        openSearch();
      }
    });
  }

  main.querySelectorAll('pre > code').forEach((code) => {
    const language = [...code.classList].find((value) => value.startsWith('language-'))?.slice(9);
    const labels = { bash: 'Shell', sh: 'Shell', shell: 'Shell', hurl: 'Hurl / HTTP + assertions', json: 'JSON', yaml: 'YAML', text: 'Text', python: 'Python', javascript: 'JavaScript' };
    if (language) code.parentElement.dataset.language = labels[language] || language;
  });

  // mdBook uses anchors without hrefs for chapter folds. Give them button semantics.
  document.querySelectorAll('.chapter a.toggle, .chapter a.chapter-fold-toggle').forEach((toggle) => {
    const chapter = toggle.closest('li.chapter-item');
    const label = chapter?.querySelector('a[href]')?.textContent.trim() || 'chapter';
    toggle.setAttribute('role', 'button');
    const sync = () => {
      const expanded = chapter?.classList.contains('expanded') || false;
      toggle.setAttribute('aria-expanded', String(expanded));
      toggle.setAttribute('aria-label', `${expanded ? 'Collapse' : 'Expand'} ${label}`);
    };
    toggle.addEventListener('click', () => queueMicrotask(sync));
    toggle.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        toggle.click();
      }
    });
    sync();
  });

  const active = document.querySelector('.chapter a.active');
  if (active) {
    active.setAttribute('aria-current', 'page');
    let previous = active.closest('li');
    while (previous && !previous.classList.contains('part-title')) previous = previous.previousElementSibling;
    if (!previous) {
      const chapter = active.closest('li.chapter-item');
      let top = chapter;
      while (top?.parentElement?.closest('li.chapter-item')) top = top.parentElement.closest('li.chapter-item');
      previous = top?.previousElementSibling;
      while (previous && !previous.classList.contains('part-title')) previous = previous.previousElementSibling;
    }
    const breadcrumb = make('div', 'rb-breadcrumb');
    const home = make('a', '', 'Docs');
    home.href = `${root}index.html`;
    breadcrumb.append(home, document.createTextNode(` / ${previous?.textContent.trim() || 'Welcome'}`));
    main.prepend(breadcrumb);
  }

  const headings = [...main.querySelectorAll('h2[id], h3[id]')];
  if (headings.filter((heading) => heading.tagName === 'H2').length > 1) {
    const toc = make('details', 'rb-page-toc');
    const wide = window.matchMedia('(min-width: 1440px)');
    toc.open = wide.matches;
    wide.addEventListener('change', (event) => { toc.open = event.matches; });
    toc.append(make('summary', '', 'On this page'), make('div', 'rb-toc-title', 'On this page'));
    const nav = make('nav');
    nav.setAttribute('aria-label', 'On this page');
    const list = make('ul');
    const links = new Map();
    headings.forEach((heading) => {
      const li = make('li', heading.tagName === 'H3' ? 'rb-toc-sub' : '');
      const link = make('a', '', heading.textContent.trim());
      link.href = `#${heading.id}`;
      links.set(heading.id, link);
      li.append(link);
      list.append(li);
    });
    nav.append(list);
    toc.append(nav);
    // Keep the hero together; otherwise put the outline after the introduction.
    const firstSection = main.querySelector('h2');
    firstSection.before(toc);
    const updateSection = () => {
      let current = headings[0];
      for (const heading of headings) {
        if (heading.getBoundingClientRect().top <= 120) current = heading;
        else break;
      }
      links.forEach((link, id) => {
        if (id === current.id) link.setAttribute('aria-current', 'location');
        else link.removeAttribute('aria-current');
      });
    };
    let scheduled = false;
    window.addEventListener('scroll', () => {
      if (scheduled) return;
      scheduled = true;
      requestAnimationFrame(() => { updateSection(); scheduled = false; });
    }, { passive: true });
    updateSection();
  }

  // Give chapter navigation meaningful names instead of two floating arrows.
  document.querySelectorAll('a.mobile-nav-chapters, .mobile-nav-chapters a').forEach((link) => {
    const previous = link.classList.contains('previous');
    const title = link.getAttribute('title') || link.getAttribute('aria-label') || '';
    const target = [...document.querySelectorAll('.chapter a')].find((item) => item.href === link.href);
    const label = target?.textContent.trim() || title.replace(/^(Previous|Next) chapter:?\s*/i, '');
    link.setAttribute('aria-label', `${previous ? 'Previous' : 'Next'}: ${label}`);
    link.replaceChildren(make('span', 'rb-nav-label', previous ? 'Previous' : 'Next'), make('span', '', label));
  });

  const footer = make('footer', 'rb-footer');
  footer.append(make('span', '', 'Rustybin / The field guide'));
  [['Find a task', 'tasks.html'], ['Troubleshooting', 'troubleshooting.html']].forEach(([label, path]) => {
    const link = make('a', '', label);
    link.href = root + path;
    footer.append(link);
  });
  const edit = document.querySelector('a[rel="edit"]');
  if (edit) {
    const link = make('a', '', 'Improve this page');
    link.href = edit.href;
    footer.append(link);
  }
  main.append(footer);
})();
