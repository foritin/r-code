import '@testing-library/jest-dom/vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App.jsx';

describe('Charter attention future demo', () => {
  beforeEach(() => {
    vi.stubGlobal('requestAnimationFrame', vi.fn(() => 1));
    vi.stubGlobal('cancelAnimationFrame', vi.fn());
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('switches among three genuinely different product shells', async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(screen.getByRole('heading', { name: 'What should we build?' })).toBeInTheDocument();
    expect(screen.getByLabelText('Chat Session request')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Next attention/ }));
    expect(screen.getAllByText('Running & restorable sessions').length).toBeGreaterThan(0);

    await user.click(screen.getByRole('tab', { name: /持久任务房间/ }));
    expect(screen.getByRole('heading', { name: 'What should we build?' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Next attention/ }));
    expect(screen.getByText('Restored where you left off')).toBeInTheDocument();
    expect(screen.getByText('Session context')).toBeInTheDocument();

    await user.click(screen.getByRole('tab', { name: /Terminal 工作台/ }));
    expect(screen.getByRole('heading', { name: 'What should we build?' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Next attention/ }));
    expect(screen.getByText('Agent context')).toBeInTheDocument();
    expect(screen.getAllByText('Search output').length).toBeGreaterThan(0);
  });

  it('keeps Home as the primary Chat Session entry in every direction', async () => {
    const user = userEvent.setup();
    render(<App />);

    const request = screen.getByLabelText('Chat Session request');
    await user.clear(request);
    await user.type(request, 'Fix the compiler test failures');
    await user.click(screen.getByRole('button', { name: /Start Chat Session/ }));

    expect(screen.getByText('进入会话与精确现场')).toBeInTheDocument();
    expect(screen.getAllByText('Which API version should we target?').length).toBeGreaterThan(0);
  });

  it('supports chapter seeking, next-attention navigation, and restart', async () => {
    const user = userEvent.setup();
    render(<App />);

    fireEvent.change(screen.getByLabelText('Demo progress'), { target: { value: '22' } });
    expect(screen.getByText('边界内审批')).toBeInTheDocument();
    expect(screen.getAllByText('Allow npm test in /compiler-lab?').length).toBeGreaterThan(0);

    await user.click(screen.getByRole('button', { name: 'Restart' }));
    expect(screen.getByText('从 Home 发起 Chat Session')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'What should we build?' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Next attention/ }));
    expect(screen.getByText('进入会话与精确现场')).toBeInTheDocument();
  });

  it('changes local approval state without broadening permission copy', async () => {
    const user = userEvent.setup();
    render(<App />);

    fireEvent.change(screen.getByLabelText('Demo progress'), { target: { value: '22' } });
    await user.click(screen.getByRole('button', { name: /Approve once/ }));

    expect(screen.getByText('Command approved once')).toBeInTheDocument();
    expect(screen.getByText('The exact command is running. No broader permission was granted.')).toBeInTheDocument();
    expect(screen.getByText('read screen, send keys')).toBeInTheDocument();
  });
});
