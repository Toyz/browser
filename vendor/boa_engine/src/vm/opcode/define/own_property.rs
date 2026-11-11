use crate::{
    Context, JsNativeError, JsResult,
    object::internal_methods::InternalMethodPropertyContext,
    property::PropertyDescriptor,
    vm::opcode::{IndexOperand, Operation, RegisterOperand},
};

/// `DefineOwnPropertyByName` implements the Opcode Operation for `Opcode::DefineOwnPropertyByName`
///
/// Operation:
///  - Defines a own property of an object by name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByName;

impl DefineOwnPropertyByName {
    #[inline(always)]
    pub(crate) fn operation(
        (object, value, index): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let object = context.vm.get_register(object.into()).clone();
        let value = context.vm.get_register(value.into()).clone();
        let name = context
            .vm
            .frame()
            .code_block()
            .constant_string(index.into());
        let object = object.to_object(context)?;
        let key: crate::property::PropertyKey = name.into();
        // (Feather) An object literal's field: on an ordinary, extensible
        // object without it yet, ordinary [[DefineOwnProperty]] just adds
        // it. Added directly (the generic path validated a descriptor
        // against a missing one, a sixth of object-heavy scripts' time).
        {
            use crate::object::internal_methods::ORDINARY_INTERNAL_METHODS;
            let vtable = object.vtable();
            if std::ptr::fn_addr_eq(vtable.__define_own_property__, ORDINARY_INTERNAL_METHODS.__define_own_property__)
                && std::ptr::fn_addr_eq(vtable.__get_own_property__, ORDINARY_INTERNAL_METHODS.__get_own_property__)
            {
                let mut o = object.borrow_mut();
                if o.extensible && !o.properties().contains_key(&key) {
                    o.insert(
                        key,
                        PropertyDescriptor::builder().value(value).writable(true).enumerable(true).configurable(true).build(),
                    );
                    return Ok(());
                }
            }
        }
        object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByName {
    const NAME: &'static str = "DefineOwnPropertyByName";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByName";
    const COST: u8 = 4;
}

/// `DefineOwnPropertyByValue` implements the Opcode Operation for `Opcode::DefineOwnPropertyByValue`
///
/// Operation:
///  - Defines a own property of an object by value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByValue;

impl DefineOwnPropertyByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, object): (RegisterOperand, RegisterOperand, RegisterOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let object = context.vm.get_register(object.into()).clone();
        let object = object.to_object(context)?;
        let key = key.to_property_key(context)?;
        let success = object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        if !success {
            return Err(JsNativeError::typ()
                .with_message("failed to defined own property")
                .into());
        }
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByValue {
    const NAME: &'static str = "DefineOwnPropertyByValue";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByValue";
    const COST: u8 = 4;
}
