use std::collections::HashMap;
use std::env;

use rustc::ty::layout::{Size};
use rustc_mir::interpret::{Pointer, Memory};
use crate::stacked_borrows::Tag;
use crate::*;

#[derive(Default)]
pub struct EnvVars {
    /// Stores pointers to the environment variables. These variables must be stored as
    /// null-terminated C strings with the `"{name}={value}"` format.
    map: HashMap<Vec<u8>, Pointer<Tag>>,
}

impl EnvVars {
    pub(crate) fn init<'mir, 'tcx>(
        ecx: &mut InterpCx<'mir, 'tcx, Evaluator<'tcx>>,
        mut excluded_env_vars: Vec<String>,
    ) {
        // Exclude `TERM` var to avoid terminfo trying to open the termcap file.
        excluded_env_vars.push("TERM".to_owned());

        if ecx.machine.communicate {
            for (name, value) in env::vars() {
                if !excluded_env_vars.contains(&name) {
                    let var_ptr = alloc_env_var(name.as_bytes(), value.as_bytes(), ecx.memory_mut());
                    ecx.machine.env_vars.map.insert(name.into_bytes(), var_ptr);
                }
            }
        }
    }
}

fn alloc_env_var<'mir, 'tcx>(
    name: &[u8],
    value: &[u8],
    memory: &mut Memory<'mir, 'tcx, Evaluator<'tcx>>,
) -> Pointer<Tag> {
    let mut bytes = name.to_vec();
    bytes.push(b'=');
    bytes.extend_from_slice(value);
    bytes.push(0);
    memory.allocate_static_bytes(bytes.as_slice(), MiriMemoryKind::Env.into())
}

impl<'mir, 'tcx> EvalContextExt<'mir, 'tcx> for crate::MiriEvalContext<'mir, 'tcx> {}
pub trait EvalContextExt<'mir, 'tcx: 'mir>: crate::MiriEvalContextExt<'mir, 'tcx> {
    fn getenv(
        &mut self,
        name_op: OpTy<'tcx, Tag>,
    ) -> InterpResult<'tcx, Scalar<Tag>> {
        let this = self.eval_context_mut();

        let name_ptr = this.read_scalar(name_op)?.not_undef()?;
        let name = this.memory().read_c_str(name_ptr)?;
        Ok(match this.machine.env_vars.map.get(name) {
            // The offset is used to strip the "{name}=" part of the string.
            Some(var_ptr) => Scalar::Ptr(var_ptr.offset(Size::from_bytes(name.len() as u64 + 1), this)?),
            None => Scalar::ptr_null(&*this.tcx),
        })
    }

    fn setenv(
        &mut self,
        name_op: OpTy<'tcx, Tag>,
        value_op: OpTy<'tcx, Tag>,
    ) -> InterpResult<'tcx, i32> {
        let this = self.eval_context_mut();

        let name_ptr = this.read_scalar(name_op)?.not_undef()?;
        let value_ptr = this.read_scalar(value_op)?.not_undef()?;
        let value = this.memory().read_c_str(value_ptr)?;
        let mut new = None;
        if !this.is_null(name_ptr)? {
            let name = this.memory().read_c_str(name_ptr)?;
            if !name.is_empty() && !name.contains(&b'=') {
                new = Some((name.to_owned(), value.to_owned()));
            }
        }
        if let Some((name, value)) = new {
            let var_ptr = alloc_env_var(&name, &value, this.memory_mut());
            if let Some(var) = this.machine.env_vars.map.insert(name.to_owned(), var_ptr) {
                this.memory_mut().deallocate(var, None, MiriMemoryKind::Env.into())?;
            }
            Ok(0)
        } else {
            Ok(-1)
        }
    }

    fn unsetenv(
        &mut self,
        name_op: OpTy<'tcx, Tag>,
    ) -> InterpResult<'tcx, i32> {
        let this = self.eval_context_mut();

        let name_ptr = this.read_scalar(name_op)?.not_undef()?;
        let mut success = None;
        if !this.is_null(name_ptr)? {
            let name = this.memory().read_c_str(name_ptr)?.to_owned();
            if !name.is_empty() && !name.contains(&b'=') {
                success = Some(this.machine.env_vars.map.remove(&name));
            }
        }
        if let Some(old) = success {
            if let Some(var) = old {
                this.memory_mut().deallocate(var, None, MiriMemoryKind::Env.into())?;
            }
            Ok(0)
        } else {
            Ok(-1)
        }
    }

    fn getcwd(
        &mut self,
        buf_op: OpTy<'tcx, Tag>,
        size_op: OpTy<'tcx, Tag>,
    ) -> InterpResult<'tcx, Scalar<Tag>> {
        let this = self.eval_context_mut();

        if !this.machine.communicate {
            throw_unsup_format!("Function not available when isolation is enabled")
        }

        let tcx = &{this.tcx.tcx};

        let buf = this.force_ptr(this.read_scalar(buf_op)?.not_undef()?)?;
        let size = this.read_scalar(size_op)?.to_usize(&*this.tcx)?;
        // If we cannot get the current directory, we return null
        // FIXME: Technically we have to set the `errno` global too
        match env::current_dir() {
            Ok(cwd) =>{
                // It is not clear what happens with non-utf8 paths here
                let mut bytes = cwd.display().to_string().into_bytes();
                // If the buffer is smaller or equal than the path, we return null.
                if (bytes.len() as u64) < size {
                    // We add a `/0` terminator
                    bytes.push(0);
                    // This is ok because the buffer is larger than the path with the null terminator.
                    this.memory_mut().get_mut(buf.alloc_id)?.write_bytes(tcx, buf, &bytes)?;
                    return Ok(Scalar::Ptr(buf))
                }
                this.machine.last_error = 34; // ERANGE
            }
            Err(e) => this.machine.last_error = e.raw_os_error().unwrap() as u32,
        }
        Ok(Scalar::ptr_null(&*this.tcx))
    }
}
