// ---------------------------------------------------------------------------
// TowerMenu — the small popup shown when a tower is clicked, replacing the
// old click-to-delete behavior with a choice: delete, or watch its beacon
// activity (see docs/design/MESH_COMMS_DESIGN.md §3 "Beacon wavefront").
//
// Plain DOM, no Cesium import — same separation InspectorPanel keeps. Unlike
// InspectorPanel (a fixed corner panel), this is a floating popup at the
// click's screen position, since it only ever needs to offer two actions for
// whichever tower was just clicked.
// ---------------------------------------------------------------------------
export class TowerMenu {
  constructor() {
    this._onDelete = null;
    this._onToggleWatch = null;

    this.menu = document.createElement('div');
    this.menu.style.cssText = `
      position: fixed; z-index: 1100; display: none; flex-direction: column;
      gap: 2px; min-width: 170px;
      background: rgba(20, 20, 20, 0.92); color: #fff;
      font: 12px sans-serif; padding: 4px; border-radius: 6px;
      border: 1px solid rgba(63, 208, 255, 0.5);
    `;
    document.body.appendChild(this.menu);

    // Delegated: buttons are rebuilt fresh on every show().
    this.menu.addEventListener('click', (e) => {
      const action = e.target.dataset && e.target.dataset.action;
      if (action === 'delete') this._onDelete && this._onDelete();
      else if (action === 'watch') this._onToggleWatch && this._onToggleWatch();
      this.hide();
    });

    this._outsideClickHandler = (e) => {
      if (!this.menu.contains(e.target)) this.hide();
    };
    this._keyHandler = (e) => {
      if (e.key === 'Escape') this.hide();
    };
  }

  show(x, y, { onDelete, onToggleWatch, isWatching }) {
    this._onDelete = onDelete;
    this._onToggleWatch = onToggleWatch;
    this.menu.innerHTML = `
      <button data-action="delete" style="text-align:left; padding:5px 8px; cursor:pointer;">
        Delete tower
      </button>
      <button data-action="watch" style="text-align:left; padding:5px 8px; cursor:pointer;">
        ${isWatching ? '◉ Stop watching beacons' : '○ View beacon activity'}
      </button>
    `;
    this.menu.style.left = `${x}px`;
    this.menu.style.top = `${y}px`;
    this.menu.style.display = 'flex';
    // Deferred: Cesium's pick fires on pointerup, but the native `click` this
    // same physical click eventually produces still bubbles to `document`
    // afterward. Attaching synchronously would catch that trailing `click`
    // as an "outside click" and close the menu the instant it opens.
    setTimeout(() => {
      document.addEventListener('click', this._outsideClickHandler, { capture: true });
      document.addEventListener('keydown', this._keyHandler);
    }, 0);
  }

  hide() {
    this.menu.style.display = 'none';
    document.removeEventListener('click', this._outsideClickHandler, { capture: true });
    document.removeEventListener('keydown', this._keyHandler);
  }
}
