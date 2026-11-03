// Apply the saved palette before styles and the app load, including on login.
try {
  if (localStorage.getItem('plombir_git_theme') === 'light') {
    document.documentElement.dataset.theme = 'light';
    document.documentElement.style.colorScheme = 'light';
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', '#ffffff');
  }
} catch {
  // Storage can be disabled; the default dark palette remains usable.
}
