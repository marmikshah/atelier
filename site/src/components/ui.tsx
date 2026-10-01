import * as Dialog from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import { useRef } from 'react';
import type { ReactNode } from 'react';
import type { Model, Provider } from '../lib/comparison.ts';
import { providers } from '../lib/comparison.ts';

export function Brand() {
  return (
    <a className="brand" href="./" aria-label="Atelier home">
      <span className="brand-mark" aria-hidden="true">
        <svg viewBox="0 0 32 32">
          <path d="M8 24V12h4V8h8v4h4v12h-4v-8h-8v8z" fill="currentColor" />
        </svg>
      </span>
      <span>
        atelier<span className="brand-period">.</span>
      </span>
    </a>
  );
}

export function ProviderMark({ provider, small = false }: { provider: Provider; small?: boolean }) {
  const value = providers.find((value) => value.id === provider)!;
  return (
    <span className={`provider-mark ${provider}${small ? ' small' : ''}`} aria-hidden="true">
      {value.initial}
    </span>
  );
}

export function ProviderLabel({ model }: { model: Model }) {
  return (
    <span className={`provider-label ${model.provider}`}>
      <span className="provider-dot" />
      {model.vendor}
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
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  label?: string;
  className?: string;
  children: ReactNode;
}) {
  const returnFocus = useRef<HTMLElement | null>(null);
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="modal-overlay" />
        <Dialog.Content
          className={`modal ${className}`}
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
