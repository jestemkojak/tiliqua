#[doc = "Register `transaction_reg` reader"]
pub type R = crate::R<TRANSACTION_REG_SPEC>;
#[doc = "Register `transaction_reg` writer"]
pub type W = crate::W<TRANSACTION_REG_SPEC>;
#[doc = "Field `data` writer - data field"]
pub type DATA_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `rw` writer - rw field"]
pub type RW_W<'a, REG> = crate::BitWriter<'a, REG>;
#[doc = "Field `last` writer - last field"]
pub type LAST_W<'a, REG> = crate::BitWriter<'a, REG>;
impl W {
    #[doc = "Bits 0:7 - data field"]
    #[inline(always)]
    pub fn data(&mut self) -> DATA_W<'_, TRANSACTION_REG_SPEC> {
        DATA_W::new(self, 0)
    }
    #[doc = "Bit 8 - rw field"]
    #[inline(always)]
    pub fn rw(&mut self) -> RW_W<'_, TRANSACTION_REG_SPEC> {
        RW_W::new(self, 8)
    }
    #[doc = "Bit 9 - last field"]
    #[inline(always)]
    pub fn last(&mut self) -> LAST_W<'_, TRANSACTION_REG_SPEC> {
        LAST_W::new(self, 9)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`transaction_reg::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`transaction_reg::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct TRANSACTION_REG_SPEC;
impl crate::RegisterSpec for TRANSACTION_REG_SPEC {
    type Ux = u16;
}
#[doc = "`read()` method returns [`transaction_reg::R`](R) reader structure"]
impl crate::Readable for TRANSACTION_REG_SPEC {}
#[doc = "`write(|w| ..)` method takes [`transaction_reg::W`](W) writer structure"]
impl crate::Writable for TRANSACTION_REG_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets transaction_reg to value 0"]
impl crate::Resettable for TRANSACTION_REG_SPEC {}
