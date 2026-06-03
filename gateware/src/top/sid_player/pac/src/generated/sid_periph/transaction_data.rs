#[doc = "Register `transaction_data` reader"]
pub type R = crate::R<TRANSACTION_DATA_SPEC>;
#[doc = "Register `transaction_data` writer"]
pub type W = crate::W<TRANSACTION_DATA_SPEC>;
#[doc = "Field `transaction_data` writer - transaction_data field"]
pub type TRANSACTION_DATA_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
impl W {
    #[doc = "Bits 0:15 - transaction_data field"]
    #[inline(always)]
    pub fn transaction_data(&mut self) -> TRANSACTION_DATA_W<'_, TRANSACTION_DATA_SPEC> {
        TRANSACTION_DATA_W::new(self, 0)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`transaction_data::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`transaction_data::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct TRANSACTION_DATA_SPEC;
impl crate::RegisterSpec for TRANSACTION_DATA_SPEC {
    type Ux = u16;
}
#[doc = "`read()` method returns [`transaction_data::R`](R) reader structure"]
impl crate::Readable for TRANSACTION_DATA_SPEC {}
#[doc = "`write(|w| ..)` method takes [`transaction_data::W`](W) writer structure"]
impl crate::Writable for TRANSACTION_DATA_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets transaction_data to value 0"]
impl crate::Resettable for TRANSACTION_DATA_SPEC {}
