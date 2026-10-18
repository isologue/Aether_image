import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useClipboard } from './useClipboard'

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))
vi.mock('./useToast', () => ({ useToast: () => ({ success: toast.success, error: toast.error }) }))
let originalSecureContext: PropertyDescriptor | undefined
let originalExecCommand: PropertyDescriptor | undefined

describe('useClipboard', () => {
  beforeEach(() => {
    originalSecureContext = Object.getOwnPropertyDescriptor(window, 'isSecureContext')
    originalExecCommand = Object.getOwnPropertyDescriptor(document, 'execCommand')
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
    if (originalSecureContext) Object.defineProperty(window, 'isSecureContext', originalSecureContext)
    else Reflect.deleteProperty(window, 'isSecureContext')
    if (originalExecCommand) Object.defineProperty(document, 'execCommand', originalExecCommand)
    else Reflect.deleteProperty(document, 'execCommand')
  })

  it('falls back to execCommand when the Clipboard API rejects', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('permission denied'))
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    Object.defineProperty(window, 'isSecureContext', { configurable: true, value: true })
    const execCommand = vi.fn().mockReturnValue(true)
    Object.defineProperty(document, 'execCommand', { configurable: true, value: execCommand })

    const { copyToClipboard } = useClipboard()
    await expect(copyToClipboard('secret')).resolves.toBe(true)

    expect(writeText).toHaveBeenCalledWith('secret')
    expect(execCommand).toHaveBeenCalledWith('copy')
    expect(toast.success).toHaveBeenCalledOnce()
    expect(toast.error).not.toHaveBeenCalled()
  })
})
