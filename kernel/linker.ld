ENTRY(kernel_main)

/* 仮想アドレス空間の基底 */
KERNEL_VMA = 0xFFFF800000000000;
KERNEL_LMA = 0x100000;

SECTIONS
{
    /* 仮想アドレスは高位、物理アドレスは低位 */
    . = KERNEL_VMA + KERNEL_LMA;

    .text ALIGN(4K) : AT(KERNEL_LMA)
    {
        __text_start = .;
        *(.text .text.*)
        *(.ltext .ltext.*)
        __text_end = .;
    }

    .rodata ALIGN(4K) : AT(ALIGN(LOADADDR(.text) + SIZEOF(.text), 4K))
    {
        __rodata_start = .;
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
        __rodata_end = .;
    }

    .eh_frame_hdr ALIGN(4K) : AT(ALIGN(LOADADDR(.rodata) + SIZEOF(.rodata), 4K))
    {
        *(.eh_frame_hdr)
    }

    .data ALIGN(4K) : AT(ALIGN(LOADADDR(.eh_frame_hdr) + SIZEOF(.eh_frame_hdr), 4K))
    {
        __data_start = .;
        *(.got)
        *(.data .data.*)
        *(.ldata .ldata.*)
        __data_end = .;
    }

    .bss ALIGN(4K) : AT(ALIGN(LOADADDR(.data) + SIZEOF(.data), 4K))
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(.lbss .lbss.*)
        *(COMMON)
        __bss_end = .;
    }

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) }
}
