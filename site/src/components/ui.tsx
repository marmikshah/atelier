import * as Dialog from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import { useRef } from 'react';
import type { KeyboardEvent, MouseEvent, ReactNode } from 'react';
import wordmark from '../assets/wordmark.png';
import type { Model } from '../lib/comparison.ts';
import { effortKey, effortName } from '../lib/comparison.ts';

export function Brand() {
  return (
    <a className="brand" href="./" aria-label="Atelier home">
      <img src={wordmark} width={151} height={48} alt="atelier" />
    </a>
  );
}

/** Follow a link normally with a modifier key; otherwise run the in-page action. */
export function inspectLink(event: MouseEvent<HTMLAnchorElement>, action: () => void) {
  if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  event.preventDefault();
  action();
}

/** A model's wall label: provider, name, and recorded effort. */
export function Placard({ model }: { model: Model }) {
  return (
    <span className="placard">
      <span className={`pigment ${model.provider}`}>{model.vendor}</span>
      <span className="placard-name">{model.name}</span>
      <span className={`effort effort-${effortKey(model)}`}>
        {model.effort ? `${model.effort} effort` : `effort ${effortName('none')}`}
      </span>
    </span>
  );
}

export function Modal({
  open,
  onOpenChange,
  title,
  description,
  label,
  className = '',
  onKeyDown,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  label?: string;
  className?: string;
  onKeyDown?: (event: KeyboardEvent) => void;
  children: ReactNode;
}) {
  const returnFocus = useRef<HTMLElement | null>(null);
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="modal-overlay" />
        <Dialog.Content
          className={`modal ${className}`}
          onKeyDown={onKeyDown}
          onOpenAutoFocus={() => {
            returnFocus.current =
              document.activeElement instanceof HTMLElement ? document.activeElement : null;
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            returnFocus.current?.focus();
          }}
        >
          <div className="modal-header">
            <div>
              {label && <p className="eyebrow">{label}</p>}
              <Dialog.Title>{title}</Dialog.Title>
            </div>
            <Dialog.Close className="icon-button" aria-label="Close dialog">
              <X size={18} aria-hidden="true" />
            </Dialog.Close>
          </div>
          <Dialog.Description className="visually-hidden">{description}</Dialog.Description>
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
