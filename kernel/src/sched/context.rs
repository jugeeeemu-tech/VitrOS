//! CPUコンテキストとコンテキストスイッチ
//!
//! このモジュールはCPUコンテキストの保存・復元とコンテキストスイッチを担当します。

use super::task::TaskError;

const STACK_SLOT_SIZE: u64 = 8;

#[inline(never)]
extern "C" fn task_entry_returned() -> ! {
    panic!("task entry returned unexpectedly");
}

fn push_stack_u64(rsp: &mut u64, value: u64) -> Result<(), TaskError> {
    *rsp = rsp
        .checked_sub(STACK_SLOT_SIZE)
        .ok_or(TaskError::InvalidStackAddress)?;
    unsafe {
        core::ptr::write(*rsp as *mut u64, value);
    }
    Ok(())
}

/// CPUコンテキスト（レジスタ状態）
///
/// Linux方式: すべてのレジスタとFPU/SSE状態をスタックに保存
/// Contextにはスタックポインタのみを保持
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    // スタックポインタのみ
    // レジスタはすべてスタックに保存される
    pub rsp: u64,
}

impl Context {
    /// 新しいコンテキストを作成（Linux方式）
    ///
    /// スタックに以下の順序でレジスタを配置（switch_context()のpush順序に合わせる）:
    /// 1. entry_pointが戻った時の戻り先（task_entry_returned） - 最上位
    /// 2. 戻りアドレス（entry_point） - switch_context()のret用
    /// 3. rbp, rbx, r12, r13, r14, r15（callee-savedレジスタ）
    /// 4. rflags
    /// 5. fxsave領域（512バイト、16バイトアライメント） - 最下位、rspがここを指す
    ///
    /// この2段の戻り先を積むことで、新規タスクの関数入口でも通常のcallと同じ
    /// `%rsp % 16 == 8` を満たし、x86_64 SysV ABIに従う。
    ///
    /// # Arguments
    /// * `entry_point` - タスクのエントリポイント
    /// * `stack_top` - スタックの最上位アドレス
    ///
    /// # Errors
    /// * `TaskError::InvalidStackAddress` - スタックアドレスが無効（null、アラインメント不正、範囲不正）
    /// * `TaskError::ContextInitFailed` - コンテキスト初期化に失敗
    pub fn new(entry_point: u64, stack_top: u64) -> Result<Self, TaskError> {
        const FXSAVE_SIZE: u64 = 512;
        const FXSAVE_ALIGN: u64 = 16;
        const MIN_REQUIRED_STACK: u64 = 1024; // 最小スタックサイズ

        // バリデーション: スタックトップがnullでないか
        if stack_top == 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // バリデーション: エントリポイントがnullでないか
        if entry_point == 0 {
            return Err(TaskError::ContextInitFailed);
        }

        // バリデーション: スタックトップが16バイトアラインされているか
        if stack_top % FXSAVE_ALIGN != 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // バリデーション: スタックが最低限の容量を持っているか
        if stack_top < MIN_REQUIRED_STACK {
            return Err(TaskError::InvalidStackAddress);
        }

        // スタックポインタの初期位置
        let mut rsp = stack_top;

        // SAFETY: 以下のunsafeブロックでは、stack_topから始まるスタック領域に
        // 初期コンテキストを書き込む。呼び出し元がstack_topが有効なスタック領域の
        // 最上位アドレスであることを保証する。

        // 1. entry_pointが戻った時の戻り先
        push_stack_u64(&mut rsp, task_entry_returned as *const () as usize as u64)?;

        // 2. 戻りアドレス（entry_point）- switch_context()のret用
        push_stack_u64(&mut rsp, entry_point)?;

        // 3. callee-savedレジスタ（switch_context()のpush順序に合わせる）
        // rbp（push rbpで積まれる）
        push_stack_u64(&mut rsp, 0)?;
        // rbx（push rbxで積まれる）
        push_stack_u64(&mut rsp, 0)?;
        // r12（push r12で積まれる）
        push_stack_u64(&mut rsp, 0)?;
        // r13（push r13で積まれる）
        push_stack_u64(&mut rsp, 0)?;
        // r14（push r14で積まれる）
        push_stack_u64(&mut rsp, 0)?;
        // r15（push r15で積まれる）
        push_stack_u64(&mut rsp, 0)?;

        // 4. rflags（pushfqで積まれる）- 割り込み有効
        push_stack_u64(&mut rsp, 0x202)?; // IF (Interrupt Flag) を有効化

        // 5. fxsave領域を確保（512バイト）
        // switch_contextと同じアラインメント処理を適用
        let rsp_before_fxsave = rsp; // アラインメント前のRSPを保存
        rsp -= FXSAVE_SIZE;
        rsp = (rsp / FXSAVE_ALIGN) * FXSAVE_ALIGN; // 16バイトアラインに切り下げ

        // バリデーション: 最終的なrspが有効かつ16バイトアラインされているか
        if rsp == 0 || rsp % FXSAVE_ALIGN != 0 {
            return Err(TaskError::InvalidStackAddress);
        }

        // fxsave領域をゼロクリア（初期状態）
        unsafe {
            core::ptr::write_bytes(rsp as *mut u8, 0, FXSAVE_SIZE as usize);
            // アラインメント前のRSPを保存（switch_contextと同じ位置）
            *((rsp + 504) as *mut u64) = rsp_before_fxsave;
        }

        Ok(Self { rsp })
    }

    /// 空のコンテキストを作成
    #[allow(dead_code)]
    pub const fn empty() -> Self {
        Self { rsp: 0 }
    }
}

/// コンテキストスイッチを実行（Linux方式）
///
/// すべてのレジスタとFPU/SSE状態をスタックに保存/復元します。
///
/// # Safety
/// この関数は低レベルのアセンブリ操作を行うため、正しいコンテキスト構造体へのポインタを渡す必要があります。
///
/// # Arguments
/// * `old_context` - 現在のコンテキストを保存する先（rspのみ）
/// * `new_context` - 切り替え先のコンテキスト（rspのみ）
///
/// # Note
/// 保存されるRFLAGSは割り込み有効フラグ(IF)が強制的にセットされます。
/// これにより、タスク復帰時に必ず割り込み有効状態になることが保証されます。
#[unsafe(naked)]
pub unsafe extern "C" fn switch_context(old_context: *mut Context, new_context: *const Context) {
    core::arch::naked_asm!(
        // ========== 現在のコンテキストを保存 ==========
        // callee-savedレジスタをスタックに保存
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // RFLAGSを保存し、IFフラグを強制的にセット
        // これにより、タスク復帰時に必ず割り込みが有効になる
        "pushfq",
        "or qword ptr [rsp], 0x200", // IF (bit 9) を強制的に1にする
        // fxsave用の領域を確保し、16バイトアラインを保証
        // call命令で8バイトプッシュされているため、アラインメント調整が必要
        "mov r11, rsp", // アラインメント前のRSPを保存
        "sub rsp, 512",
        "and rsp, -16", // 16バイトアラインに切り下げ
        // FPU/SSE状態を保存
        "fxsave [rsp]",
        // アラインメント前のRSPをスタックに保存（復元時に必要）
        "mov [rsp + 504], r11", // fxsave領域の末尾近くに保存
        // 現在のrspをold_contextに保存
        "mov [rdi], rsp",
        // ========== 新しいコンテキストを復元 ==========
        // new_context->rspを読み込み
        "mov rsp, [rsi]",
        // FPU/SSE状態を復元
        "fxrstor [rsp]",
        // アラインメント前のRSPを復元
        "mov rsp, [rsp + 504]",
        // RFLAGSを復元（IFは保存時に強制セット済みなので、割り込み有効で復帰）
        "popfq",
        // callee-savedレジスタを復元（保存と逆順）
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        // リターン（スタックトップの戻りアドレスに戻る）
        "ret",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_STACK_SIZE: usize = 2048;

    #[repr(align(16))]
    struct TestStack([u8; TEST_STACK_SIZE]);

    extern "C" fn sample_task_entry() -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    #[test_case]
    fn test_new_context_starts_task_with_sysv_stack_alignment() {
        let mut stack = TestStack([0; TEST_STACK_SIZE]);
        let stack_top = stack.0.as_mut_ptr() as u64 + TEST_STACK_SIZE as u64;

        let context =
            Context::new(sample_task_entry as *const () as usize as u64, stack_top).unwrap();

        let rsp_before_fxsave = unsafe { *((context.rsp + 504) as *const u64) };
        let entry_slot = rsp_before_fxsave + 7 * STACK_SLOT_SIZE;
        let return_slot = entry_slot + STACK_SLOT_SIZE;
        let function_entry_rsp = entry_slot + STACK_SLOT_SIZE;

        assert_eq!(context.rsp % 16, 0);
        assert_eq!(rsp_before_fxsave % 16, 8);
        assert_eq!(unsafe { *(rsp_before_fxsave as *const u64) }, 0x202);
        assert_eq!(
            unsafe { *(entry_slot as *const u64) },
            sample_task_entry as *const () as usize as u64
        );
        assert_eq!(
            unsafe { *(return_slot as *const u64) },
            task_entry_returned as *const () as usize as u64
        );
        assert_eq!(function_entry_rsp % 16, 8);
    }

    #[test_case]
    fn test_new_context_rejects_unaligned_stack_top() {
        let mut stack = TestStack([0; TEST_STACK_SIZE]);
        let stack_top = stack.0.as_mut_ptr() as u64 + TEST_STACK_SIZE as u64 - STACK_SLOT_SIZE;

        let err =
            Context::new(sample_task_entry as *const () as usize as u64, stack_top).unwrap_err();

        assert_eq!(err, TaskError::InvalidStackAddress);
    }
}
