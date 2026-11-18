import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import './index.css'
import './app.css'

// The page is an app, not a document: no pinch zoom (Safari still offers it despite the
// viewport tag) and no ctrl/⌘ + wheel or trackpad-pinch zoom on desktop.
for (const type of ['gesturestart', 'gesturechange', 'gestureend']) {
  document.addEventListener(type, (event) => event.preventDefault(), { passive: false })
}
document.addEventListener(
  'wheel',
  (event) => {
    if (event.ctrlKey || event.metaKey) event.preventDefault()
  },
  { passive: false },
)

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
