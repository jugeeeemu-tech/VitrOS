use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::hpet;
#[cfg(feature = "visualize-input")]
use crate::input_trace::TraceStatusSnapshot;
use crate::timer;
use crate::usb;
use crate::usb::device::UsbDeviceInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEffect {
    AppendLines(Vec<String>),
    ClearOutput,
    #[cfg(feature = "visualize-input")]
    Visualization(VisualizationCommand),
}

#[cfg(feature = "visualize-input")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizationTarget {
    Input,
}

#[cfg(feature = "visualize-input")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizationAction {
    On,
    Off,
    Clear,
}

#[cfg(feature = "visualize-input")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualizationCommand {
    pub target: VisualizationTarget,
    pub action: VisualizationAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub effects: Vec<CommandEffect>,
}

impl CommandOutcome {
    fn empty() -> Self {
        Self {
            effects: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CommandExecutor;

impl CommandExecutor {
    pub const fn new() -> Self {
        Self
    }

    pub fn execute_line(&self, line: &str) -> CommandOutcome {
        self.execute_line_with_context(line, &LiveBuiltinContext)
    }

    fn execute_line_with_context(
        &self,
        line: &str,
        context: &dyn BuiltinContext,
    ) -> CommandOutcome {
        let Some(parsed) = parse_command(line) else {
            return CommandOutcome::empty();
        };

        let Some(spec) = builtin_spec(parsed.name) else {
            return unknown_command(parsed.name);
        };

        if spec.arg_policy == ArgPolicy::NoArgs && !parsed.args.is_empty() {
            return usage_error(spec.usage);
        }

        (spec.handler)(context, &parsed.args)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCommand<'a> {
    name: &'a str,
    args: Vec<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgPolicy {
    NoArgs,
    Variadic,
}

type BuiltinHandler = fn(&dyn BuiltinContext, &[&str]) -> CommandOutcome;

#[derive(Clone, Copy)]
struct BuiltinCommandSpec {
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    arg_policy: ArgPolicy,
    handler: BuiltinHandler,
}

trait BuiltinContext {
    fn uptime_ms(&self) -> Option<u64>;
    fn current_tick(&self) -> u64;
    fn timer_frequency_hz(&self) -> u64;
    fn snapshot_devices(&self) -> Vec<UsbDeviceInfo>;
    #[cfg(feature = "visualize-input")]
    fn input_trace_status(&self) -> TraceStatusSnapshot;
}

struct LiveBuiltinContext;

impl BuiltinContext for LiveBuiltinContext {
    fn uptime_ms(&self) -> Option<u64> {
        if hpet::is_available() {
            Some(hpet::elapsed_ms())
        } else {
            None
        }
    }

    fn current_tick(&self) -> u64 {
        timer::current_tick()
    }

    fn timer_frequency_hz(&self) -> u64 {
        timer::frequency_hz()
    }

    fn snapshot_devices(&self) -> Vec<UsbDeviceInfo> {
        usb::snapshot_devices()
    }

    #[cfg(feature = "visualize-input")]
    fn input_trace_status(&self) -> TraceStatusSnapshot {
        crate::input_trace::status_snapshot()
    }
}

const BUILTIN_COMMANDS: &[BuiltinCommandSpec] = &[
    BuiltinCommandSpec {
        name: "help",
        usage: "help",
        summary: "list available built-in commands",
        arg_policy: ArgPolicy::NoArgs,
        handler: help_command,
    },
    BuiltinCommandSpec {
        name: "clear",
        usage: "clear",
        summary: "clear shell output history",
        arg_policy: ArgPolicy::NoArgs,
        handler: clear_command,
    },
    BuiltinCommandSpec {
        name: "echo",
        usage: "echo [args...]",
        summary: "print arguments as a single line",
        arg_policy: ArgPolicy::Variadic,
        handler: echo_command,
    },
    BuiltinCommandSpec {
        name: "uptime",
        usage: "uptime",
        summary: "show HPET-based uptime",
        arg_policy: ArgPolicy::NoArgs,
        handler: uptime_command,
    },
    BuiltinCommandSpec {
        name: "ticks",
        usage: "ticks",
        summary: "show timer tick diagnostics",
        arg_policy: ArgPolicy::NoArgs,
        handler: ticks_command,
    },
    BuiltinCommandSpec {
        name: "devices",
        usage: "devices",
        summary: "list enumerated USB devices",
        arg_policy: ArgPolicy::NoArgs,
        handler: devices_command,
    },
    #[cfg(feature = "visualize-input")]
    BuiltinCommandSpec {
        name: "visualize",
        usage: "visualize [input] [on|off|status|clear]",
        summary: "control visualization features",
        arg_policy: ArgPolicy::Variadic,
        handler: visualize_command,
    },
];

fn parse_command(line: &str) -> Option<ParsedCommand<'_>> {
    let mut tokens = line.split_ascii_whitespace();
    let name = tokens.next()?;
    let args = tokens.collect();
    Some(ParsedCommand { name, args })
}

fn builtin_spec(name: &str) -> Option<&'static BuiltinCommandSpec> {
    BUILTIN_COMMANDS.iter().find(|spec| spec.name == name)
}

fn help_command(_context: &dyn BuiltinContext, _args: &[&str]) -> CommandOutcome {
    let mut lines = Vec::with_capacity(BUILTIN_COMMANDS.len() + 1);
    lines.push(String::from("built-in commands:"));
    for spec in BUILTIN_COMMANDS {
        lines.push(format!("{} - {}", spec.usage, spec.summary));
    }
    append_lines(lines)
}

#[cfg(feature = "visualize-input")]
fn visualize_command(context: &dyn BuiltinContext, args: &[&str]) -> CommandOutcome {
    if args.is_empty() {
        let mut lines = Vec::with_capacity(3);
        lines.push(String::from("visualization targets:"));
        lines.push(format_status_line("input", context.input_trace_status()));
        lines.push(String::from("usage: visualize input <on|off|status|clear>"));
        return append_lines(lines);
    }

    match args[0] {
        "input" => visualize_input_command(context, &args[1..]),
        target => {
            let mut lines = Vec::with_capacity(2);
            lines.push(format!("unknown visualization target: {}", target));
            lines.push(String::from("available targets: input"));
            append_lines(lines)
        }
    }
}

#[cfg(feature = "visualize-input")]
fn visualize_input_command(context: &dyn BuiltinContext, args: &[&str]) -> CommandOutcome {
    let status = context.input_trace_status();
    match args {
        [] | ["status"] => append_lines(status_lines(status)),
        ["on"] => visualization_outcome(
            TraceStatusSnapshot {
                enabled: true,
                ..status
            },
            VisualizationAction::On,
        ),
        ["off"] => visualization_outcome(
            TraceStatusSnapshot {
                enabled: false,
                ..status
            },
            VisualizationAction::Off,
        ),
        ["clear"] => visualization_outcome(
            TraceStatusSnapshot {
                stored_records: 0,
                dropped_records: 0,
                ..status
            },
            VisualizationAction::Clear,
        ),
        _ => append_lines(vec![String::from(
            "usage: visualize input <on|off|status|clear>",
        )]),
    }
}

#[cfg(feature = "visualize-input")]
fn status_lines(status: TraceStatusSnapshot) -> Vec<String> {
    let mut lines = Vec::with_capacity(8);
    lines.push(format!(
        "input visualization: {}",
        if status.enabled { "on" } else { "off" }
    ));
    lines.push(String::from("mode: controller diagram"));
    lines.push(String::from("keyboard path: controller-centric"));
    lines.push(String::from("modules: os,xhci,keyboard,transfer,dma,event"));
    lines.push(format!(
        "active keyboard: {}",
        if status.active_keyboard_present {
            "present"
        } else {
            "absent"
        }
    ));
    lines.push(format!(
        "controller snapshot: {}",
        if status.controller_snapshot_available {
            "available"
        } else {
            "unavailable"
        }
    ));
    lines.push(format!("stored traces: {}", status.stored_records));
    lines.push(format!("dropped traces: {}", status.dropped_records));
    lines
}

#[cfg(feature = "visualize-input")]
fn format_status_line(target: &str, status: TraceStatusSnapshot) -> String {
    format!(
        "{} - {} (stored={}, dropped={})",
        target,
        if status.enabled { "on" } else { "off" },
        status.stored_records,
        status.dropped_records
    )
}

#[cfg(feature = "visualize-input")]
fn visualization_outcome(
    status: TraceStatusSnapshot,
    action: VisualizationAction,
) -> CommandOutcome {
    let mut effects = Vec::with_capacity(2);
    effects.push(CommandEffect::AppendLines(status_lines(status)));
    effects.push(CommandEffect::Visualization(VisualizationCommand {
        target: VisualizationTarget::Input,
        action,
    }));
    CommandOutcome { effects }
}

fn clear_command(_context: &dyn BuiltinContext, _args: &[&str]) -> CommandOutcome {
    CommandOutcome {
        effects: vec![CommandEffect::ClearOutput],
    }
}

fn echo_command(_context: &dyn BuiltinContext, args: &[&str]) -> CommandOutcome {
    let mut line = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        line.push_str(arg);
    }
    append_lines(vec![line])
}

fn uptime_command(context: &dyn BuiltinContext, _args: &[&str]) -> CommandOutcome {
    let Some(uptime_ms) = context.uptime_ms() else {
        return append_lines(vec![String::from("uptime: HPET unavailable")]);
    };

    let seconds = uptime_ms / 1000;
    let millis = uptime_ms % 1000;
    append_lines(vec![format!("uptime: {}.{:03}s (HPET)", seconds, millis)])
}

fn ticks_command(context: &dyn BuiltinContext, _args: &[&str]) -> CommandOutcome {
    let mut lines = Vec::with_capacity(2);
    lines.push(format!("tick count: {}", context.current_tick()));
    let frequency_hz = context.timer_frequency_hz();
    if frequency_hz == 0 {
        lines.push(String::from("timer frequency: unavailable"));
    } else {
        lines.push(format!("timer frequency: {} Hz", frequency_hz));
    }
    append_lines(lines)
}

fn devices_command(context: &dyn BuiltinContext, _args: &[&str]) -> CommandOutcome {
    let mut devices = context.snapshot_devices();
    devices.sort_by_key(|device| (device.port_id, device.address, device.handle.as_u64()));

    if devices.is_empty() {
        return append_lines(vec![String::from("no usb devices")]);
    }

    let mut lines = Vec::new();
    for (index, device) in devices.iter().enumerate() {
        lines.push(format_device_summary(index, device));

        if device.configurations.is_empty() {
            lines.push(String::from("  no configurations"));
            continue;
        }

        for configuration in &device.configurations {
            lines.push(format_configuration_summary(configuration));

            if configuration.interfaces.is_empty() {
                lines.push(String::from("    no interfaces"));
                continue;
            }

            for interface in &configuration.interfaces {
                lines.push(format_interface_summary(interface));

                if interface.endpoints.is_empty() {
                    lines.push(String::from("      no endpoints"));
                    continue;
                }

                for endpoint in &interface.endpoints {
                    lines.push(format_endpoint_summary(endpoint));
                }
            }
        }
    }

    append_lines(lines)
}

fn format_device_summary(index: usize, device: &UsbDeviceInfo) -> String {
    format!(
        "device {}: port={} address={} speed={} vid=0x{:04X} pid=0x{:04X}",
        index + 1,
        device.port_id,
        device.address,
        device.speed.as_str(),
        device.vendor_id,
        device.product_id
    )
}

fn format_configuration_summary(
    configuration: &crate::usb::device::UsbConfigurationInfo,
) -> String {
    format!(
        "  config {}: attributes=0x{:02X} max_power={}mA",
        configuration.configuration_value,
        configuration.attributes,
        u16::from(configuration.max_power) * 2
    )
}

fn format_interface_summary(interface: &crate::usb::device::UsbInterfaceInfo) -> String {
    format!(
        "    interface {} alt={} class=0x{:02X} subclass=0x{:02X} protocol=0x{:02X}",
        interface.number,
        interface.alternate_setting,
        interface.class,
        interface.subclass,
        interface.protocol
    )
}

fn format_endpoint_summary(endpoint: &crate::usb::device::UsbEndpointInfo) -> String {
    format!(
        "      endpoint 0x{:02X} attr=0x{:02X} mps={} interval={}",
        endpoint.address, endpoint.attributes, endpoint.max_packet_size, endpoint.interval
    )
}

fn usage_error(usage: &str) -> CommandOutcome {
    append_lines(vec![format!("usage: {}", usage)])
}

fn unknown_command(name: &str) -> CommandOutcome {
    append_lines(vec![
        format!("unknown command: {}", name),
        String::from("run 'help' to list built-in commands"),
    ])
}

fn append_lines(lines: Vec<String>) -> CommandOutcome {
    CommandOutcome {
        effects: vec![CommandEffect::AppendLines(lines)],
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{CommandEffect, CommandExecutor, CommandOutcome, ParsedCommand, parse_command};
    #[cfg(feature = "visualize-input")]
    use super::{VisualizationAction, VisualizationCommand, VisualizationTarget};
    #[cfg(feature = "visualize-input")]
    use crate::input_trace::TraceStatusSnapshot;
    use crate::usb::device::{
        UsbConfigurationInfo, UsbDeviceHandle, UsbDeviceInfo, UsbEndpointInfo, UsbInterfaceInfo,
        UsbSpeed,
    };

    #[derive(Clone, Default)]
    struct FakeContext {
        uptime_ms: Option<u64>,
        tick_count: u64,
        timer_frequency_hz: u64,
        devices: Vec<UsbDeviceInfo>,
        #[cfg(feature = "visualize-input")]
        input_trace_status: TraceStatusSnapshot,
    }

    impl super::BuiltinContext for FakeContext {
        fn uptime_ms(&self) -> Option<u64> {
            self.uptime_ms
        }

        fn current_tick(&self) -> u64 {
            self.tick_count
        }

        fn timer_frequency_hz(&self) -> u64 {
            self.timer_frequency_hz
        }

        fn snapshot_devices(&self) -> Vec<UsbDeviceInfo> {
            self.devices.clone()
        }

        #[cfg(feature = "visualize-input")]
        fn input_trace_status(&self) -> TraceStatusSnapshot {
            self.input_trace_status
        }
    }

    fn parse(line: &str) -> ParsedCommand<'_> {
        parse_command(line).expect("command should parse")
    }

    fn execute_with_context(line: &str, context: &FakeContext) -> CommandOutcome {
        CommandExecutor::new().execute_line_with_context(line, context)
    }

    fn appended_lines(outcome: CommandOutcome) -> Vec<String> {
        match outcome
            .effects
            .into_iter()
            .next()
            .expect("at least one effect")
        {
            CommandEffect::AppendLines(lines) => lines,
            CommandEffect::ClearOutput => panic!("expected appended lines"),
            #[cfg(feature = "visualize-input")]
            CommandEffect::Visualization(_) => panic!("expected appended lines"),
        }
    }

    fn device_with_details(port_id: u8, address: u8) -> UsbDeviceInfo {
        UsbDeviceInfo {
            handle: UsbDeviceHandle::allocate(),
            port_id,
            address,
            speed: UsbSpeed::High,
            vendor_id: 0x1234,
            product_id: 0x5678,
            configurations: vec![UsbConfigurationInfo {
                configuration_value: 1,
                attributes: 0xA0,
                max_power: 50,
                interfaces: vec![UsbInterfaceInfo {
                    number: 0,
                    alternate_setting: 0,
                    class: 0x03,
                    subclass: 0x01,
                    protocol: 0x01,
                    endpoints: vec![UsbEndpointInfo {
                        address: 0x81,
                        attributes: 0x03,
                        max_packet_size: 8,
                        interval: 10,
                    }],
                }],
            }],
        }
    }

    #[test_case]
    fn test_parse_command_returns_none_for_empty_input() {
        assert_eq!(parse_command(""), None);
    }

    #[test_case]
    fn test_parse_command_returns_none_for_ascii_whitespace_only_input() {
        assert_eq!(parse_command(" \t \n "), None);
    }

    #[test_case]
    fn test_parse_command_splits_ascii_whitespace() {
        let parsed = parse("echo\talpha   beta\n gamma");
        assert_eq!(parsed.name, "echo");
        assert_eq!(parsed.args, vec!["alpha", "beta", "gamma"]);
    }

    #[test_case]
    fn test_parse_command_keeps_quotes_as_literal_characters() {
        let parsed = parse("echo \"a b\"");
        assert_eq!(parsed.name, "echo");
        assert_eq!(parsed.args, vec!["\"a", "b\""]);
    }

    #[test_case]
    fn test_parse_command_keeps_symbols_literal() {
        let parsed = parse("echo a|b foo>bar $baz\\qux");
        assert_eq!(parsed.name, "echo");
        assert_eq!(parsed.args, vec!["a|b", "foo>bar", "$baz\\qux"]);
    }

    #[test_case]
    fn test_execute_line_returns_noop_for_empty_input() {
        let outcome = execute_with_context("", &FakeContext::default());
        assert!(outcome.effects.is_empty());
    }

    #[test_case]
    fn test_execute_line_returns_noop_for_whitespace_only_input() {
        let outcome = execute_with_context("   \t ", &FakeContext::default());
        assert!(outcome.effects.is_empty());
    }

    #[test_case]
    fn test_help_lists_all_builtins_in_registration_order() {
        let lines = appended_lines(execute_with_context("help", &FakeContext::default()));
        #[cfg(not(feature = "visualize-input"))]
        assert_eq!(
            lines,
            vec![
                String::from("built-in commands:"),
                String::from("help - list available built-in commands"),
                String::from("clear - clear shell output history"),
                String::from("echo [args...] - print arguments as a single line"),
                String::from("uptime - show HPET-based uptime"),
                String::from("ticks - show timer tick diagnostics"),
                String::from("devices - list enumerated USB devices"),
            ]
        );
        #[cfg(feature = "visualize-input")]
        assert_eq!(
            lines,
            vec![
                String::from("built-in commands:"),
                String::from("help - list available built-in commands"),
                String::from("clear - clear shell output history"),
                String::from("echo [args...] - print arguments as a single line"),
                String::from("uptime - show HPET-based uptime"),
                String::from("ticks - show timer tick diagnostics"),
                String::from("devices - list enumerated USB devices"),
                String::from(
                    "visualize [input] [on|off|status|clear] - control visualization features"
                ),
            ]
        );
    }

    #[test_case]
    fn test_help_rejects_extra_arguments() {
        let lines = appended_lines(execute_with_context("help extra", &FakeContext::default()));
        assert_eq!(lines, vec![String::from("usage: help")]);
    }

    #[test_case]
    fn test_clear_returns_clear_output_effect_only() {
        let outcome = execute_with_context("clear", &FakeContext::default());
        assert_eq!(outcome.effects, vec![CommandEffect::ClearOutput]);
    }

    #[test_case]
    fn test_clear_rejects_extra_arguments() {
        let lines = appended_lines(execute_with_context("clear now", &FakeContext::default()));
        assert_eq!(lines, vec![String::from("usage: clear")]);
    }

    #[cfg(feature = "visualize-input")]
    #[test_case]
    fn test_visualize_lists_available_target() {
        let context = FakeContext {
            input_trace_status: TraceStatusSnapshot {
                enabled: false,
                stored_records: 2,
                dropped_records: 1,
                generation: 7,
                ..TraceStatusSnapshot::default()
            },
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("visualize", &context));
        assert_eq!(
            lines,
            vec![
                String::from("visualization targets:"),
                String::from("input - off (stored=2, dropped=1)"),
                String::from("usage: visualize input <on|off|status|clear>"),
            ]
        );
    }

    #[cfg(feature = "visualize-input")]
    #[test_case]
    fn test_visualize_input_on_returns_status_and_effect() {
        let context = FakeContext {
            input_trace_status: TraceStatusSnapshot {
                enabled: false,
                stored_records: 0,
                dropped_records: 0,
                generation: 0,
                ..TraceStatusSnapshot::default()
            },
            ..FakeContext::default()
        };
        let outcome = execute_with_context("visualize input on", &context);
        assert_eq!(
            outcome.effects,
            vec![
                CommandEffect::AppendLines(vec![
                    String::from("input visualization: on"),
                    String::from("mode: diagram line-light"),
                    String::from("keyboard path: ring-focused"),
                    String::from("rings: transfer,event"),
                    String::from("active keyboard: absent"),
                    String::from("controller snapshot: unavailable"),
                    String::from("stored traces: 0"),
                    String::from("dropped traces: 0"),
                ]),
                CommandEffect::Visualization(VisualizationCommand {
                    target: VisualizationTarget::Input,
                    action: VisualizationAction::On,
                }),
            ]
        );
    }

    #[cfg(feature = "visualize-input")]
    #[test_case]
    fn test_visualize_input_clear_resets_displayed_counts() {
        let context = FakeContext {
            input_trace_status: TraceStatusSnapshot {
                enabled: true,
                stored_records: 8,
                dropped_records: 3,
                generation: 12,
                ..TraceStatusSnapshot::default()
            },
            ..FakeContext::default()
        };
        let outcome = execute_with_context("visualize input clear", &context);
        assert_eq!(
            outcome.effects,
            vec![
                CommandEffect::AppendLines(vec![
                    String::from("input visualization: on"),
                    String::from("mode: diagram line-light"),
                    String::from("keyboard path: ring-focused"),
                    String::from("rings: transfer,event"),
                    String::from("active keyboard: absent"),
                    String::from("controller snapshot: unavailable"),
                    String::from("stored traces: 0"),
                    String::from("dropped traces: 0"),
                ]),
                CommandEffect::Visualization(VisualizationCommand {
                    target: VisualizationTarget::Input,
                    action: VisualizationAction::Clear,
                }),
            ]
        );
    }

    #[cfg(feature = "visualize-input")]
    #[test_case]
    fn test_visualize_input_rejects_unknown_action() {
        let lines = appended_lines(execute_with_context(
            "visualize input toggle",
            &FakeContext::default(),
        ));
        assert_eq!(
            lines,
            vec![String::from("usage: visualize input <on|off|status|clear>")]
        );
    }

    #[test_case]
    fn test_echo_joins_arguments_with_single_spaces() {
        let lines = appended_lines(execute_with_context(
            "echo hello   kernel shell",
            &FakeContext::default(),
        ));
        assert_eq!(lines, vec![String::from("hello kernel shell")]);
    }

    #[test_case]
    fn test_echo_without_arguments_returns_single_empty_line() {
        let lines = appended_lines(execute_with_context("echo", &FakeContext::default()));
        assert_eq!(lines, vec![String::new()]);
    }

    #[test_case]
    fn test_uptime_reports_hpet_based_duration_when_available() {
        let context = FakeContext {
            uptime_ms: Some(12_345),
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("uptime", &context));
        assert_eq!(lines, vec![String::from("uptime: 12.345s (HPET)")]);
    }

    #[test_case]
    fn test_uptime_reports_unavailable_without_hpet() {
        let lines = appended_lines(execute_with_context("uptime", &FakeContext::default()));
        assert_eq!(lines, vec![String::from("uptime: HPET unavailable")]);
    }

    #[test_case]
    fn test_ticks_reports_tick_count_and_frequency() {
        let context = FakeContext {
            tick_count: 512,
            timer_frequency_hz: 250,
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("ticks", &context));
        assert_eq!(
            lines,
            vec![
                String::from("tick count: 512"),
                String::from("timer frequency: 250 Hz"),
            ]
        );
    }

    #[test_case]
    fn test_ticks_reports_unavailable_frequency_when_zero() {
        let context = FakeContext {
            tick_count: 10,
            timer_frequency_hz: 0,
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("ticks", &context));
        assert_eq!(
            lines,
            vec![
                String::from("tick count: 10"),
                String::from("timer frequency: unavailable"),
            ]
        );
    }

    #[test_case]
    fn test_devices_reports_no_usb_devices() {
        let lines = appended_lines(execute_with_context("devices", &FakeContext::default()));
        assert_eq!(lines, vec![String::from("no usb devices")]);
    }

    #[test_case]
    fn test_devices_formats_hierarchical_device_details() {
        let context = FakeContext {
            devices: vec![device_with_details(1, 2)],
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("devices", &context));
        assert_eq!(
            lines,
            vec![
                String::from("device 1: port=1 address=2 speed=High vid=0x1234 pid=0x5678"),
                String::from("  config 1: attributes=0xA0 max_power=100mA"),
                String::from("    interface 0 alt=0 class=0x03 subclass=0x01 protocol=0x01"),
                String::from("      endpoint 0x81 attr=0x03 mps=8 interval=10"),
            ]
        );
    }

    #[test_case]
    fn test_devices_sorts_by_port_and_address() {
        let context = FakeContext {
            devices: vec![
                device_with_details(3, 2),
                device_with_details(1, 4),
                device_with_details(1, 1),
            ],
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("devices", &context));
        assert_eq!(
            lines[0],
            String::from("device 1: port=1 address=1 speed=High vid=0x1234 pid=0x5678")
        );
        assert_eq!(
            lines[4],
            String::from("device 2: port=1 address=4 speed=High vid=0x1234 pid=0x5678")
        );
        assert_eq!(
            lines[8],
            String::from("device 3: port=3 address=2 speed=High vid=0x1234 pid=0x5678")
        );
    }

    #[test_case]
    fn test_devices_includes_placeholders_for_empty_children() {
        let context = FakeContext {
            devices: vec![
                UsbDeviceInfo {
                    handle: UsbDeviceHandle::allocate(),
                    port_id: 1,
                    address: 1,
                    speed: UsbSpeed::Full,
                    vendor_id: 0xAAAA,
                    product_id: 0x0001,
                    configurations: Vec::new(),
                },
                UsbDeviceInfo {
                    handle: UsbDeviceHandle::allocate(),
                    port_id: 2,
                    address: 1,
                    speed: UsbSpeed::Low,
                    vendor_id: 0xBBBB,
                    product_id: 0x0002,
                    configurations: vec![UsbConfigurationInfo {
                        configuration_value: 2,
                        attributes: 0x80,
                        max_power: 25,
                        interfaces: Vec::new(),
                    }],
                },
                UsbDeviceInfo {
                    handle: UsbDeviceHandle::allocate(),
                    port_id: 3,
                    address: 1,
                    speed: UsbSpeed::Super,
                    vendor_id: 0xCCCC,
                    product_id: 0x0003,
                    configurations: vec![UsbConfigurationInfo {
                        configuration_value: 3,
                        attributes: 0xC0,
                        max_power: 10,
                        interfaces: vec![UsbInterfaceInfo {
                            number: 1,
                            alternate_setting: 1,
                            class: 0xFF,
                            subclass: 0x01,
                            protocol: 0x02,
                            endpoints: Vec::new(),
                        }],
                    }],
                },
            ],
            ..FakeContext::default()
        };
        let lines = appended_lines(execute_with_context("devices", &context));
        assert_eq!(
            lines,
            vec![
                String::from("device 1: port=1 address=1 speed=Full vid=0xAAAA pid=0x0001"),
                String::from("  no configurations"),
                String::from("device 2: port=2 address=1 speed=Low vid=0xBBBB pid=0x0002"),
                String::from("  config 2: attributes=0x80 max_power=50mA"),
                String::from("    no interfaces"),
                String::from("device 3: port=3 address=1 speed=Super vid=0xCCCC pid=0x0003"),
                String::from("  config 3: attributes=0xC0 max_power=20mA"),
                String::from("    interface 1 alt=1 class=0xFF subclass=0x01 protocol=0x02"),
                String::from("      no endpoints"),
            ]
        );
    }

    #[test_case]
    fn test_unknown_command_returns_explicit_error() {
        let lines = appended_lines(execute_with_context("unknown", &FakeContext::default()));
        assert_eq!(
            lines,
            vec![
                String::from("unknown command: unknown"),
                String::from("run 'help' to list built-in commands"),
            ]
        );
    }

    #[test_case]
    fn test_command_names_are_case_sensitive() {
        let lines = appended_lines(execute_with_context("HELP", &FakeContext::default()));
        assert_eq!(
            lines,
            vec![
                String::from("unknown command: HELP"),
                String::from("run 'help' to list built-in commands"),
            ]
        );
    }
}
