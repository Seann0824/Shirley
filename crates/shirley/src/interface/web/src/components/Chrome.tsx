import { Icon } from "./Icon";

export function StatusBar() {
  return (
    <div className="status-bar">
      <span>{new Date().toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</span>
      <div className="device-status">
        <i />
        <i />
        <b />
      </div>
    </div>
  );
}

export function BackHeader({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <header className="back-header">
      <button className="back-button" onClick={onBack} aria-label="返回">
        <Icon name="chevron" size={20} />
      </button>
      <strong>{title}</strong>
      <span className="header-spacer" />
    </header>
  );
}

export function LoadingRows({ cards = 2 }: { cards?: number }) {
  return (
    <div className="loading-rows" aria-label="正在加载">
      {Array.from({ length: cards }).map((_, index) => (
        <div className="loading-card" key={index}>
          <i />
          <span>
            <b />
            <b />
          </span>
        </div>
      ))}
      <p>
        <span />
        正在同步
      </p>
    </div>
  );
}

export function ErrorNote({ message }: { message: string }) {
  return (
    <p className="error-note" role="alert">
      {message}
    </p>
  );
}
