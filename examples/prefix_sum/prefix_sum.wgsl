struct ScanParameters {
    offset: u32,
    element_count: u32,
    padding_0: u32,
    padding_1: u32,
}

@group(0) @binding(0)
var<storage, read> source_values: array<u32>;

@group(0) @binding(1)
var<storage, read_write> destination_values: array<u32>;

@group(0) @binding(2)
var<uniform> parameters: ScanParameters;

@compute @workgroup_size(64)
fn prefix_sum(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if index >= parameters.element_count {
        return;
    }

    var value = source_values[index];
    if index >= parameters.offset {
        value += source_values[index - parameters.offset];
    }

    destination_values[index] = value;
}
