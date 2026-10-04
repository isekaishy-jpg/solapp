//! Narrow Windows query; byte parsing is bounds checked and does not cast records.

use std::mem::{offset_of, size_of};

use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows::Win32::System::SystemInformation::{
    GROUP_AFFINITY, GetLogicalProcessorInformationEx, PROCESSOR_RELATIONSHIP,
    RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use crate::{SACoreAffinity, SACpuCore, SACpuTopology, SATopologyError};

pub(crate) fn query() -> Result<SACpuTopology, SATopologyError> {
    let mut required = 0;
    // SAFETY: null buffer is the documented size query; required is writable.
    let probe =
        unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut required) };
    if let Err(error) = probe
        && error.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
    {
        return Err(native_error(error));
    }
    for _ in 0..4 {
        if required == 0 {
            return Err(SATopologyError::InvalidData("empty native topology"));
        }
        // usize storage guarantees the x64 native structure's alignment. All
        // bytes start initialized; Windows may only write the advertised length.
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut storage = Vec::<usize>::new();
        storage
            .try_reserve_exact(words)
            .map_err(|_| SATopologyError::AllocationFailed)?;
        storage.resize(words, 0);
        let capacity = required;
        // SAFETY: storage is aligned and holds at least capacity writable bytes;
        // the API receives that byte count and cannot retain this pointer.
        let result = unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorCore,
                Some(storage.as_mut_ptr().cast()),
                &mut required,
            )
        };
        match result {
            Ok(()) => {
                if required > capacity {
                    return Err(SATopologyError::InvalidData(
                        "returned length exceeds buffer",
                    ));
                }
                // SAFETY: all returned bytes are inside initialized storage;
                // parsing is immutable and ends before storage is dropped.
                let bytes = unsafe {
                    std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), required as usize)
                };
                return parse(bytes);
            }
            Err(error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {
                if required <= capacity {
                    return Err(SATopologyError::InvalidData("non-growing retry size"));
                }
            }
            Err(error) => return Err(native_error(error)),
        }
    }
    Err(SATopologyError::ChangedDuringQuery)
}

fn native_error(error: windows::core::Error) -> SATopologyError {
    SATopologyError::Native {
        code: error.code().0,
        message: error.to_string(),
    }
}

fn bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N], SATopologyError> {
    data.get(
        offset
            ..offset
                .checked_add(N)
                .ok_or(SATopologyError::InvalidData("offset overflow"))?,
    )
    .and_then(|value| value.try_into().ok())
    .ok_or(SATopologyError::InvalidData("truncated record"))
}

fn parse(mut data: &[u8]) -> Result<SACpuTopology, SATopologyError> {
    let mut cores: Vec<SACpuCore> = Vec::new();
    let mut logical_processors = 0usize;
    while !data.is_empty() {
        let relation = i32::from_le_bytes(bytes(data, 0)?);
        let size = u32::from_le_bytes(bytes(data, 4)?) as usize;
        let header = offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
        if relation != RelationProcessorCore.0 || size < header || size > data.len() {
            return Err(SATopologyError::InvalidData("invalid core record header"));
        }
        let record = &data[..size];
        let group_count = u16::from_le_bytes(bytes(
            record,
            header + offset_of!(PROCESSOR_RELATIONSHIP, GroupCount),
        )?) as usize;
        // Windows documents one group per physical core. Preserve an unexpected
        // but well-formed multi-group core rather than folding its group masks.
        if group_count == 0 {
            return Err(SATopologyError::InvalidData("core has no processor groups"));
        }
        let masks_end = header
            + offset_of!(PROCESSOR_RELATIONSHIP, GroupMask)
            + group_count * size_of::<GROUP_AFFINITY>();
        if masks_end > record.len() {
            return Err(SATopologyError::InvalidData(
                "truncated group affinity array",
            ));
        }
        let mut affinities = Vec::new();
        affinities
            .try_reserve_exact(group_count)
            .map_err(|_| SATopologyError::AllocationFailed)?;
        for index in 0..group_count {
            let base = header
                + offset_of!(PROCESSOR_RELATIONSHIP, GroupMask)
                + index * size_of::<GROUP_AFFINITY>();
            let mask = u64::from_le_bytes(bytes(record, base + offset_of!(GROUP_AFFINITY, Mask))?);
            let group =
                u16::from_le_bytes(bytes(record, base + offset_of!(GROUP_AFFINITY, Group))?);
            if mask == 0
                || affinities
                    .iter()
                    .any(|prior: &SACoreAffinity| prior.group == group)
                || cores
                    .iter()
                    .flat_map(|core| &core.affinities)
                    .any(|prior| prior.group == group && prior.mask & mask != 0)
            {
                return Err(SATopologyError::InvalidData(
                    "empty or overlapping core affinity",
                ));
            }
            logical_processors = logical_processors
                .checked_add(mask.count_ones() as usize)
                .ok_or(SATopologyError::InvalidData("processor count overflow"))?;
            affinities.push(SACoreAffinity { group, mask });
        }
        let efficiency_class = bytes::<1>(
            record,
            header + offset_of!(PROCESSOR_RELATIONSHIP, EfficiencyClass),
        )?[0];
        cores
            .try_reserve(1)
            .map_err(|_| SATopologyError::AllocationFailed)?;
        cores.push(SACpuCore {
            affinities,
            efficiency_class,
        });
        data = &data[size..];
    }
    if cores.is_empty() {
        return Err(SATopologyError::InvalidData("empty native topology"));
    }
    Ok(SACpuTopology {
        cores,
        logical_processors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(group: u16, mask: u64, efficiency: u8) -> Vec<u8> {
        let header = offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
        let size = header + size_of::<PROCESSOR_RELATIONSHIP>();
        let mut data = vec![0; size];
        data[..4].copy_from_slice(&RelationProcessorCore.0.to_le_bytes());
        data[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        let count = header + offset_of!(PROCESSOR_RELATIONSHIP, GroupCount);
        data[count..count + 2].copy_from_slice(&1u16.to_le_bytes());
        data[header + offset_of!(PROCESSOR_RELATIONSHIP, EfficiencyClass)] = efficiency;
        let affinity = header + offset_of!(PROCESSOR_RELATIONSHIP, GroupMask);
        data[affinity..affinity + 8].copy_from_slice(&mask.to_le_bytes());
        let group_offset = affinity + offset_of!(GROUP_AFFINITY, Group);
        data[group_offset..group_offset + 2].copy_from_slice(&group.to_le_bytes());
        data
    }

    #[test]
    fn groups_and_heterogeneous_cores_remain_distinct() {
        let mut data = core(0, 3, 8);
        data.extend(core(1, 3, 0));
        let topology = parse(&data).unwrap();
        assert_eq!(topology.physical_cores(), 2);
        assert_eq!(topology.logical_processors(), 4);
        assert_eq!(topology.cores()[0].efficiency_class(), 8);
        assert_eq!(topology.cores()[1].affinities()[0].group, 1);
    }

    #[test]
    fn malformed_native_records_do_not_invent_counts() {
        assert!(parse(&[]).is_err());
        let valid = core(0, 3, 0);
        for length in 0..valid.len() {
            assert!(parse(&valid[..length]).is_err());
        }
        let mut duplicated = valid.clone();
        duplicated.extend(valid.clone());
        assert!(parse(&duplicated).is_err());
        assert!(parse(&core(0, 0, 0)).is_err());
        let mut zero_size = valid;
        zero_size[4..8].fill(0);
        assert!(parse(&zero_size).is_err());
    }

    #[test]
    fn current_windows_topology_is_observable() {
        let topology = query().unwrap();
        assert!(topology.physical_cores() > 0);
        assert!(topology.logical_processors() >= topology.physical_cores());
    }
}
